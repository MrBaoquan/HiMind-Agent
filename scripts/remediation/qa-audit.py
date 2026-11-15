import os,json,hashlib
from pathlib import Path
root=Path.cwd(); out=Path(os.environ['SERVICE_UI_RUNTIME_ROOT']); h=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
allowed=['src','scripts','docs','remediation-input','reports','resources','icons','frontend/src','frontend/scripts']
singles=['Cargo.toml','Cargo.lock','build.rs','tauri.conf.json','frontend/styles.css','frontend/package.json','frontend/package-lock.json','frontend/index.html','frontend/vite.config.ts','frontend/tsconfig.json']
def authorized(n):return n in singles or any(n.startswith(d+'/') for d in allowed)
base=json.loads(Path('remediation-input/baseline-manifest.json').read_text(encoding='utf-8'))['files']; changes=[]; skipped=[]
for n,v in base.items():
 if not authorized(n): skipped.append(n);continue
 p=root/n; actual=h(p) if p.is_file() else None
 if actual!=v:changes.append({'path':n,'baseline':v,'current':actual})
inputs={str(p).replace('\\','/'):h(p) for p in Path('remediation-input').rglob('*') if p.is_file()}
expected={'remediation-input/execution-contract.md':'223f2504846212b0be923ea68557a6066e4add4e561357f656e01f49e5390dd0','remediation-input/backlog.json':'391f81822f5a40715893e580d8525e87924803dcec5b527e7dd0373a36d7f53c','remediation-input/baseline-manifest.json':'b19505f763d23bdc3f0a5a9a23ef3f90a3d5926af36768a8479152cecba4f686','remediation-input/prestart-host-patch.json':'6e3f81575f102378129e43a6be219668ff542c3e65c04f9b4452c51ffa049c0d'}
r={'baseline_count':len(base),'changes':changes,'outside_task_read_scope_not_read':skipped,'input_sha256':inputs,'known_inputs_match':all(inputs.get(k)==v for k,v in expected.items()),'expected_source':'reports/implementation-plan.md historical hashes; not a signed independent original'}
(out/'qa-baseline.json').write_text(json.dumps(r,indent=2),encoding='utf-8');print(json.dumps({'baseline_count':len(base),'changed_count':len(changes),'skipped':len(skipped),'known_inputs_match':r['known_inputs_match']}))
