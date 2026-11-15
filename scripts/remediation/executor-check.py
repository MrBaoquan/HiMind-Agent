"""Record reproducible checks under the host-owned generated resource root."""
import ctypes, hashlib, json, os, subprocess, sys, time
from pathlib import Path
sys.dont_write_bytecode=True
from executor_lifecycle_support import test_environment

workspace = Path(__file__).resolve().parents[2]
root = Path(os.environ['SERVICE_UI_RUNTIME_ROOT']).resolve(strict=True)
assert root.is_dir() and not root.is_relative_to(workspace) and not workspace.is_relative_to(root)
out = root / 'executor'
out.mkdir(exist_ok=True)
short = ctypes.create_unicode_buffer(32768)
assert ctypes.windll.kernel32.GetShortPathNameW(str(out), short, len(short))
generated = Path(short.value)
env = test_environment(root,root/'executor-lifecycle-repair','prebuild')
# Cargo canonicalizes --target-dir. MSVC link.exe still rejects long .rlib
# arguments; shorten only this task's generated paths at the linker boundary.
wrapper_source = out / 'linkwrap.rs'
wrapper_source.write_text(r'''use std::{env,process::{Command,exit}};
fn main(){
 let long=env::var("EXECUTOR_LONG_ROOT").unwrap();
 let short=env::var("EXECUTOR_SHORT_ROOT").unwrap();
 let replace=|s:String|s.replace(&long,&short).replace(&long.replace('\\',"/"),&short);
 let args:Vec<String>=env::args().skip(1).map(|a| {
  if let Some(p)=a.strip_prefix('@') {
   let bytes=std::fs::read(p).unwrap();
   let utf16=bytes.starts_with(&[255,254]);
   let data=if utf16 {String::from_utf16(&bytes[2..].chunks_exact(2).map(|b|u16::from_le_bytes([b[0],b[1]])).collect::<Vec<_>>()).unwrap()}else{String::from_utf8(bytes).unwrap()};
   let path=format!("{}\\link-{}.rsp",short,std::process::id());
   let data=replace(data);
   if utf16 {let bytes:Vec<u8>=[255,254].into_iter().chain(data.encode_utf16().flat_map(u16::to_le_bytes)).collect();std::fs::write(&path,bytes).unwrap();}
   else {std::fs::write(&path,data).unwrap();}
   format!("@{}",path)
  } else {replace(a)}
 }).collect();
 let status=Command::new("link.exe").args(args).status().unwrap();
 exit(status.code().unwrap_or(1));
}''',encoding='utf-8')
wrapper = generated/'linkwrap.exe'
compile_wrapper = subprocess.run(['rustc',str(generated/'linkwrap.rs'),'--edition=2021','-o',str(wrapper)],env=env,capture_output=True)
assert compile_wrapper.returncode == 0, compile_wrapper.stderr.decode(errors='replace')
env.update(CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=str(wrapper), EXECUTOR_LONG_ROOT=str(out), EXECUTOR_SHORT_ROOT=str(generated))
label = sys.argv[1]
command = sys.argv[2:]
assert command and all(c.isalnum() or c in '-_' for c in label)
stamp = str(time.time_ns())
env['HIMIND_AGENT_HOME'] = str(generated / f'home-{stamp}')
env.update(GIT_CONFIG_COUNT='1', GIT_CONFIG_KEY_0='core.longpaths', GIT_CONFIG_VALUE_0='true')
log = out / f'{label}-{stamp}.log'
start = time.monotonic()
with log.open('w', encoding='utf-8') as stream:
    result = subprocess.run(command, cwd=workspace, env=env, stdout=stream, stderr=subprocess.STDOUT)
record = dict(command=subprocess.list2cmdline(command), argv=command, exit_code=result.returncode,
              evidence=str(log), sha256=hashlib.sha256(log.read_bytes()).hexdigest(), elapsed_seconds=round(time.monotonic()-start,3))
(out/f'{label}-{stamp}.json').write_text(json.dumps(record, ensure_ascii=False, indent=2),encoding='utf-8')
print(json.dumps(record, ensure_ascii=False))
if result.returncode: print(log.read_text(encoding='utf-8',errors='replace')[-16000:])
raise SystemExit(result.returncode)
