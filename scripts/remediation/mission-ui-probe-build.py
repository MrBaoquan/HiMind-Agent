import ctypes, hashlib, json, os, pathlib, subprocess

workspace = pathlib.Path(__file__).resolve().parents[2]
root = pathlib.Path(os.environ['SERVICE_UI_RUNTIME_ROOT']).resolve(strict=True)
assert root.is_dir() and not root.is_relative_to(workspace) and not workspace.is_relative_to(root)
probe = root / 'probe'
probe.mkdir(exist_ok=True)
(probe / 'src').mkdir(exist_ok=True)
source = (workspace / 'scripts/remediation/mission-ui-probe.rs').read_text(encoding='utf-8')
checks=(workspace/'src/mission/checks.rs').read_text(encoding='utf-8')
validator=checks[checks.index('pub(crate) fn validate_required_check'):checks.index('/// Read only aggregate usage')]
parser=checks[checks.index('fn command_words'):checks.index('#[cfg(test)]')]
(probe/'src/checks.rs').write_text('use std::error::Error;\n'+validator+parser,encoding='utf-8')
(probe / 'src/main.rs').write_text(source.replace('@WORKSPACE@', workspace.as_posix()).replace('@CHECKS@',(probe/'src/checks.rs').as_posix()), encoding='utf-8')
(probe / 'Cargo.toml').write_text('''[package]
name="mission-ui-store-probe"
version="0.1.0"
edition="2021"
[dependencies]
serde={version="1",features=["derive"]}
serde_json="1"
sha2="0.10"
rusqlite={version="0.32",features=["bundled"]}
chrono={version="0.4",default-features=false,features=["std","clock"]}
rand="0.8"
walkdir="2.5"
''',encoding='utf-8')
inputs = ['src/mission/contract.rs','src/mission/store.rs','src/mission/candidate.rs','src/mission/checks.rs','scripts/remediation/mission-ui-probe.rs']
(probe/'inputs.json').write_text(json.dumps({p:hashlib.sha256((workspace/p).read_bytes()).hexdigest() for p in inputs},indent=2),encoding='utf-8')
short = ctypes.create_unicode_buffer(32768)
assert ctypes.windll.kernel32.GetShortPathNameW(str(probe), short, len(short))
short_probe = pathlib.Path(short.value)
assert short_probe.resolve() == probe
env = dict(os.environ, CARGO_HOME=str(short_probe.parent/'cargo-home'), CARGO_TARGET_DIR=str(short_probe/'t'), RUST_MIN_STACK='16777216', TEMP=str(short_probe), TMP=str(short_probe))
cmd=['cargo','build','--manifest-path',str(short_probe/'Cargo.toml')]
number = len(list(probe.glob('build-*.log'))) + 1
log_path = probe/f'build-{number}.log'
with log_path.open('w',encoding='utf-8') as log:
    result=subprocess.run(cmd,env=env,stdout=log,stderr=subprocess.STDOUT)
(probe/'build-result.json').write_text(json.dumps({'command':cmd,'exit_code':result.returncode,'evidence':str(log_path)}),encoding='utf-8')
print(json.dumps({'exit_code':result.returncode,'evidence':str(log_path)}))
if result.returncode:
    print(log_path.read_text(encoding='utf-8')[-12000:])
raise SystemExit(result.returncode)
