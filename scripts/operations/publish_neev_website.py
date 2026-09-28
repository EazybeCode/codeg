"""Deploy a validated static Neev homepage without restarting the app."""
import datetime, hashlib, pathlib, subprocess, sys, time, urllib.request
stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
release = pathlib.Path('/var/lib/neev-proxy/data/website/releases') / stamp
source = pathlib.Path('/tmp/neev-website-upload')
expected = sys.argv[1]
assert hashlib.sha256((source/'index.html').read_bytes()).hexdigest() == expected
allowed = {'index.html', 'styles.css', 'script.js', 'favicon.svg', 'robots.txt', 'sitemap.xml'}
assert {p.name for p in source.iterdir()} == allowed
release.mkdir(parents=True)
for part in [release.parent.parent, release.parent, release]: part.chmod(0o755)
for p in source.iterdir():
 assert p.is_file() and not p.is_symlink()
 target = release/p.name; target.write_bytes(p.read_bytes()); target.chmod(0o644)
config = pathlib.Path('/etc/neev-proxy/Caddyfile')
old = config.read_text()
start = '# --- BEGIN neev.so apex (added 2026-09-20; reversible) ---'
end = '# --- END neev.so apex ---'
assert old.count(start)==1 and old.count(end)==1
container = 'neev-native-https-20260913-https-1'
app_before = subprocess.check_output(['docker','inspect','--format','{{.Id}}','neev-native-https-20260913-neev-1'],text=True).strip()
block = '''# --- BEGIN neev.so apex (added 2026-09-20; reversible) ---
http://neev.so, http://www.neev.so {
 redir https://neev.so{uri} permanent
}
https://www.neev.so {
 redir https://neev.so{uri} permanent
}
https://neev.so {
 root * /data/website/releases/RELEASE
 encode zstd gzip
 header {
  Strict-Transport-Security "max-age=31536000"
  X-Content-Type-Options nosniff
  Referrer-Policy strict-origin-when-cross-origin
  Content-Security-Policy "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; object-src 'none'"
  Cache-Control "public, max-age=0, must-revalidate"
  -Server
 }
 @app path /workspace /workspace/* /settings /settings/* /login /login/* /api/* /neev-auth/* /document-engine/*
 redir @app https://app.neev.so{uri} temporary
 file_server
}
# --- END neev.so apex ---'''.replace('RELEASE',stamp)
candidate = old[:old.index(start)] + block + old[old.index(end)+len(end):]
backup = config.with_name('Caddyfile.before-website-'+stamp)
backup.write_text(old); backup.chmod(0o600)
path = pathlib.Path('/var/lib/neev-proxy/data/website/Caddyfile-candidate'); path.write_text(candidate);path.chmod(0o644)
subprocess.run(['docker','exec',container,'caddy','validate','--config','/data/website/Caddyfile-candidate','--adapter','caddyfile'],check=True)
try:
 # Write in place: the live container bind-mounts this inode.
 config.write_text(candidate)
 subprocess.run(['docker','kill','--signal=USR1',container],check=True,stdout=subprocess.DEVNULL)
 for attempt in range(20):
  try:
   response=urllib.request.urlopen('https://neev.so/?release='+stamp,timeout=5)
   if response.url.startswith('https://neev.so/') and hashlib.sha256(response.read()).hexdigest()==expected: break
  except Exception: pass
  time.sleep(1)
 else: raise RuntimeError('Public homepage did not match release')
 assert urllib.request.urlopen('https://app.neev.so/healthz',timeout=10).status==200
 assert subprocess.check_output(['docker','inspect','--format','{{.Id}}','neev-native-https-20260913-neev-1'],text=True).strip()==app_before
except Exception:
 config.write_text(old)
 subprocess.run(['docker','kill','--signal=USR1',container],check=True,stdout=subprocess.DEVNULL)
 raise
print('VERIFIED',stamp,expected,'app container unchanged')
