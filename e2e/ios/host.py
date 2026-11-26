#!/usr/bin/env python3
"""The host side of the iPhone e2e run (tools/ios.sh e2e): launches the e2e build in a booted
simulator, follows its stderr, and does what the page can't: screenshots (E2E-SHOT), sending the app
to the background and back (E2E-HOST background), and looking at the session file's backup flag
(E2E-HOST check-backup). Then checks what the native side logged (ios.rs) and exits non-zero on any
failure.

    host.py <udid> <bundle id> <out dir>

With E2E_PROXY=1 (the build's OBSCURIFY_API is http://127.0.0.1:8093) it also runs
e2e/flaky-proxy.mjs in front of production, so the driver can script answers that take as long as a
test needs (E2E-HOST script [...]) at no cost; everything else passes through to obscurify.ai.
"""
import json, os, re, subprocess, sys, time, urllib.request

udid, bundle, out = sys.argv[1:4]
out = os.path.abspath(out)  # simctl writes the logs from launchd, which needs an absolute path
os.makedirs(out, exist_ok=True)
err_path, out_path = os.path.join(out, 'stderr.log'), os.path.join(out, 'stdout.log')
open(err_path, 'w').close()
failures = []


def sh(*a, check=False):
    r = subprocess.run(a, capture_output=True, text=True)
    if check and r.returncode:
        raise SystemExit(f'{a}: {r.stderr}')
    return r


def note(ok, what):
    print(('ok - ' if ok else 'FAILED - ') + what, flush=True)
    if not ok:
        failures.append(what)


def ack(name):
    """The driver waits for this before it goes on (main.rs reads e2e-shot-done)."""
    with open(os.path.join(container(), 'Library', 'Application Support', 'e2e-shot-done'), 'w') as f:
        f.write(name)


def screenshot(name, then_ack=False):
    path = os.path.join(out, f'{name}.png')
    sh('xcrun', 'simctl', 'io', udid, 'screenshot', path)
    size = sh('sips', '-g', 'pixelWidth', '-g', 'pixelHeight', path).stdout.split()
    print(f'shot {name}: {size[-3]}x{size[-1]}', flush=True)
    if then_ack:
        ack(name)


def background(what, most):
    """Another app in front while an answer is worked on, until it ends (`working: off`) or `most`
    seconds; then the app comes back. Judged from the app's own log (its clock is the device's): the
    answer must have gone on with the app in the background (state 2) and ended there."""
    start, size = time.time(), os.path.getsize(err_path)
    sh('xcrun', 'simctl', 'launch', udid, 'com.apple.Preferences')
    print(f'host: {what}: the app is in the background', flush=True)
    seg = ''
    while time.time() - start < most:
        time.sleep(1)
        with open(err_path, errors='replace') as f:
            f.seek(size)
            seg = f.read()
        if 'working: off' in seg:
            break
    away = time.time() - start
    print(f'host: {what}: {away:.0f} s in the background', flush=True)
    screenshot(f'background-{what}')
    sh('xcrun', 'simctl', 'launch', udid, bundle)
    print(f'host: {what}: the app is back', flush=True)
    off = re.search(r'working: off \(app state (\d+)\)', seg)
    went = 'app state 2 (background)' in seg
    beats = len(re.findall(r'beat: dioxus', seg))
    stamps = [float(x) for x in re.findall(r'^obscurify-ios: \[([\d.]+)s\]', seg, re.M)]
    span = stamps[-1] - stamps[0] if len(stamps) > 1 else 0
    print(f'host: {what}: app log over {span:.0f} s: background={went} dioxus beats={beats} '
          f'continued={"submitted" if "continued: submitted" in seg else "no"} running={"continued: running" in seg} '
          f'ended={off.group(0) if off else "not while away"}', flush=True)
    if off and off.group(1) != '2' and not went:
        print(f'host: {what}: INCONCLUSIVE - the answer ended before the app was in the background', flush=True)
        inconclusive.append(what)
    else:
        note(bool(off) and off.group(1) == '2', f'{what}: the answer finished with the app in the background'
             + ('' if off else f' (still going after {away:.0f} s)'))


def proxy(path, body=None):
    req = urllib.request.Request('http://127.0.0.1:8093' + path, data=body.encode() if body is not None else None, method='POST' if body is not None else 'GET')
    return urllib.request.urlopen(req, timeout=10).read().decode()


inconclusive = []
proxied = os.environ.get('E2E_PROXY') == '1'
if proxied:
    log_path = os.path.join(out, 'proxy.log')
    subprocess.Popen(['node', os.path.join(os.path.dirname(__file__), '..', 'flaky-proxy.mjs')], stdout=open(log_path, 'w'), stderr=subprocess.STDOUT)
    for _ in range(40):
        try:
            proxy('/__set'); break
        except Exception:
            time.sleep(0.25)
    print('host: the proxy is up on 127.0.0.1:8093', flush=True)


def container():
    return sh('xcrun', 'simctl', 'get_app_container', udid, bundle, 'data').stdout.strip()


def running():
    return bundle in sh('xcrun', 'simctl', 'spawn', udid, 'launchctl', 'list').stdout


sh('xcrun', 'simctl', 'launch', '--terminate-running-process', f'--stdout={out_path}', f'--stderr={err_path}', udid, bundle, check=True)
deadline = time.time() + int(os.environ.get('E2E_TIMEOUT', '1800'))
seen, done = 0, None
while time.time() < deadline and done is None:
    time.sleep(0.5)
    with open(err_path, errors='replace') as f:
        lines = f.read().splitlines()
    for line in lines[seen:]:
        if line.startswith('E2E-SHOT '):
            screenshot(line.split(' ', 1)[1].strip(), then_ack=True)
        elif line.startswith('E2E-HOST background'):
            # E2E-HOST background <name> <most seconds>
            parts = line.split()
            background(parts[2] if len(parts) > 2 else 'answer', int(parts[3]) if len(parts) > 3 else 60)
            ack('host')
        elif line.startswith('E2E-HOST script '):
            if proxied:
                proxy('/__set')
                proxy('/__script', line.split(' ', 2)[2])
                print('host: scripted ' + str(len(json.loads(line.split(' ', 2)[2]))) + ' answer(s)', flush=True)
            else:
                note(False, 'a scripted answer needs E2E_PROXY=1')
            ack('host')
        elif line.startswith('E2E-HOST check-backup'):
            support = os.path.join(container(), 'Library', 'Application Support')
            chats = sorted(n for n in os.listdir(support) if n.startswith('chat-') and n.endswith('.json'))
            print(f'host: {len(chats)} chat files', flush=True)
            for name in ['session.json', 'unlinkable-seed.json', 'chats.json', 'settings.json'] + chats[:1]:
                path = os.path.join(support, name)
                if not os.path.exists(path):
                    print(f'host: no {name}', flush=True)
                    continue
                attrs = sh('xattr', path).stdout
                excluded = 'com_apple_backup_excludeItem' in attrs
                secret = name != 'settings.json'  # session, keys and chats stay out; settings go in
                note(excluded == secret, f'{name} is {"not " if not excluded else ""}excluded from backups')
            ack('host')
        elif line.startswith('E2E-DONE'):
            done = line.split()[-1]
        elif line.startswith('E2E ') or line.startswith('obscurify-ios:'):
            print(line, flush=True)
        if line.startswith('E2E FAILED'):
            failures.append(line[len('E2E FAILED - '):])
    seen = len(lines)
    if done is None and not running():
        time.sleep(2)
        if not running():
            failures.append('the app stopped running')
            break

log = open(err_path, errors='replace').read()
note(done is not None, 'the driver finished' if done else 'the driver never finished')
# What the native side (ios.rs) said it did.
note('sandbox: runner loaded' in log, 'Code Mode: the sandbox WKWebView loaded the runner')
note('page: open' in log, 'Code Mode: a page opened full screen')
note('working: off (app state 2)' in log, 'an answer finished with the app in the background' if 'working: off (app state 2)' in log else 'no answer finished in the background')
if inconclusive:
    print('inconclusive (the answer ended before the app left the screen): ' + ', '.join(inconclusive), flush=True)
if proxied:
    print('proxy log:\n' + open(log_path, errors='replace').read()[-3000:], flush=True)
status = next((l.split('authorization status ')[1] for l in log.splitlines() if 'authorization status' in l), '?')
note('notified: answer ready' in log or "couldn't notify" in log, 'the answer-ready notification was ' + (
    'handed to iOS' if 'notified: answer ready' in log else 'refused') + f' (authorization status {status}: 2 would show it; the e2e build never asks)')
for line in log.splitlines():
    if 'backup:' in line:
        print(line, flush=True)
print(f'{len(failures)} failure(s)' + (': ' + '; '.join(failures) if failures else ''), flush=True)
sys.exit(1 if failures else 0)
