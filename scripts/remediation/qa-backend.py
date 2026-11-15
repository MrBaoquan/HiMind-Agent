import os,json,subprocess,hashlib,re,shutil,sys
from pathlib import Path
sys.dont_write_bytecode=True
from executor_lifecycle_support import test_environment,run
root=Path(os.environ['SERVICE_UI_RUNTIME_ROOT']);out=root/'qa-backend';out.mkdir(exist_ok=True)
logs=list((root/'executor').glob('qa-build-*.log')); assert len(logs)==1
artifacts=[json.loads(l) for l in logs[0].read_text(encoding='utf-8',errors='replace').splitlines() if l.startswith('{')]
binaries=[a['executable'] for a in artifacts if a.get('reason')=='compiler-artifact' and a.get('executable') and a['target']['name']=='himind-agent' and a['profile'].get('test')];assert len(binaries)==1
binary=out/'qa-tests.exe';shutil.copy2(binaries[0],binary)
groups={'config':['store::ai_services','store::atomic_file'],'gateway':['app::inference_gateway::','app::chat_responses::'],'budget':['mission::store::budget_remediation_tests::'],'mission':['mission::store::tests::'],'supervisor':['app::mission_supervisor::tests::'],'handoff':['executor_handoff','executor_bidirectional_handoff'],'usage':['runtime::codex_usage::tests::','--skip','usage_restart_worker']}
records=[]
for label,args in groups.items():
 rec,text=run(root,out,label,[str(binary),*args,'--nocapture','--test-threads=1'],120)
 summaries=re.findall(r'test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored',text)
 rec['passed']=rec['exit_code']==0 and bool(summaries) and all(s=='ok' and int(p)>0 and int(f)==int(i)==0 for s,p,f,i in summaries)
 rec['observed_tests']=re.findall(r'^test ([\w:]+) \.\.\.',text,re.M);rec['summary']=summaries;records.append(rec)
 (out/'result.json').write_text(json.dumps({'passed':all(r['passed'] for r in records),'checks':records},indent=2),encoding='utf-8')
 print(json.dumps(rec),flush=True)
sys.exit(0 if all(r['passed'] for r in records) else 1)
