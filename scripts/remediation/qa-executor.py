import sys,os,json,time,importlib.util
from pathlib import Path
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('repair',Path(__file__).with_name('executor-lifecycle-repair-checks.py'));mod=importlib.util.module_from_spec(spec);spec.loader.exec_module(mod)
root=Path(os.environ['SERVICE_UI_RUNTIME_ROOT']);out=root/'qa-backend';binary=out/'qa-tests.exe'
r,text=mod.run(root,out,'full-inventory',[str(binary),'--list','--format','terse'],10)
names=[l[:-6] for l in text.splitlines() if l.endswith(': test')];assert r['exit_code']==0 and names
inventory={label:mod.planned_tests(names,args) for label,args in mod.GROUPS}
start=time.monotonic();checks=list(mod.product_groups(root,out,binary,inventory,start+47,'independent-full-'))
result={'passed':all(c['passed'] for c in checks) and time.monotonic()-start<47,'checks':checks,'elapsed_ms':round((time.monotonic()-start)*1000),'live_model_handoff':'not executed; no producer cache was read'}
(out/'executor-result.json').write_text(json.dumps(result,indent=2),encoding='utf-8');print(json.dumps({'passed':result['passed'],'groups':len(checks),'tests':sum(c['test_count'] for c in checks),'failures':[c['name'] for c in checks if not c['passed']]}));sys.exit(0 if result['passed'] else 1)
