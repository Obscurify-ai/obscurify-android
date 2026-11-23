#!/usr/bin/env python3
"""The host side of the iPhone e2e run (tools/ios.sh e2e): launches the e2e build in a booted
simulator, follows its stderr, and does what the page can't: screenshots (E2E-SHOT), sending the app
to the background and back (E2E-HOST background), and looking at the session file's backup flag
(E2E-HOST check-backup). Then checks what the native side logged (ios.rs) and exits non-zero on any
failure.

    host.py <udid> <bundle id> <out dir>
"""
import os, subprocess, sys, time

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


def screenshot(name, ack=False):
    path = os.path.join(out, f'{name}.png')
    sh('xcrun', 'simctl', 'io', udid, 'screenshot', path)
    size = sh('sips', '-g', 'pixelWidth', '-g', 'pixelHeight', path).stdout.split()
    print(f'shot {name}: {size[-3]}x{size[-1]}', flush=True)
    if ack:  # the driver waits for this before it goes on
        with open(os.path.join(container(), 'Library', 'Application Support', 'e2e-shot-done'), 'w') as f:
            f.write(name)


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
            screenshot(line.split(' ', 1)[1].strip(), ack=True)
        elif line.startswith('E2E-HOST background'):
            # Another app in front: ours is in the background while its answer is worked on.
            sh('xcrun', 'simctl', 'launch', udid, 'com.apple.Preferences')
            print('host: the app is in the background', flush=True)
            time.sleep(25)
            screenshot('background')
            sh('xcrun', 'simctl', 'launch', udid, bundle)
            print('host: the app is back', flush=True)
        elif line.startswith('E2E-HOST check-backup'):
            support = os.path.join(container(), 'Library', 'Application Support')
            for name in ('session.json', 'unlinkable-seed.json', 'chats.json', 'settings.json'):
                path = os.path.join(support, name)
                if not os.path.exists(path):
                    print(f'host: no {name}', flush=True)
                    continue
                attrs = sh('xattr', path).stdout
                excluded = 'com_apple_backup_excludeItem' in attrs
                secret = name in ('session.json', 'unlinkable-seed.json')
                note(excluded == secret, f'{name} is {"not " if not excluded else ""}excluded from backups')
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
note('working: off (app state 2)' in log or 'working: off (app state 1)' in log,
     'an answer finished with the app in the background' if 'app state 2)' in log or 'app state 1)' in log else 'no answer finished in the background')
note('notified: answer ready' in log or "couldn't notify" in log, 'the answer-ready notification was posted (' + (
    'shown' if 'notified: answer ready' in log else 'refused: notifications not allowed in this simulator') + ')')
for line in log.splitlines():
    if 'backup:' in line:
        print(line, flush=True)
print(f'{len(failures)} failure(s)' + (': ' + '; '.join(failures) if failures else ''), flush=True)
sys.exit(1 if failures else 0)
