#!/usr/bin/env python3
"""A device .ipa made ready for `xtool install` on the test iPhone (test installs only, never the store).

xtool re-signs with its own team and renames the bundle to XTL-<team>.<bundle id>; a continued
processing task's identifier has to start with the bundle id (src/ios.rs), so the renamed id's
`<id>.answer.*` is added to BGTaskSchedulerPermittedIdentifiers.

    tools/ios-xtool-ipa.py Obscurify.ipa out.ipa XTL-97QU7SP64C.ai.obscurify.ios
"""
import os, plistlib, shutil, sys, tempfile, zipfile

src, dst, bundle = sys.argv[1:4]
work = tempfile.mkdtemp()
with zipfile.ZipFile(src) as z:
    z.extractall(work)
    infos = z.infolist()
plist = os.path.join(work, 'Payload', 'Obscurify.app', 'Info.plist')
with open(plist, 'rb') as f:
    p = plistlib.load(f)
ids = p.get('BGTaskSchedulerPermittedIdentifiers', [])
if f'{bundle}.answer.*' not in ids:
    ids.append(f'{bundle}.answer.*')
p['BGTaskSchedulerPermittedIdentifiers'] = ids
with open(plist, 'wb') as f:
    plistlib.dump(p, f, fmt=plistlib.FMT_BINARY)
with zipfile.ZipFile(dst, 'w', zipfile.ZIP_DEFLATED) as out:
    for i in infos:
        if i.is_dir():
            out.writestr(i, b'')
        else:
            with open(os.path.join(work, i.filename), 'rb') as f:
                out.writestr(i, f.read())  # the ZipInfo keeps the executable bit
shutil.rmtree(work)
print('BGTaskSchedulerPermittedIdentifiers =', ids, '->', dst)
