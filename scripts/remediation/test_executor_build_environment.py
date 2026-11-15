"""Bounded cross-process regressions of the real effective identity guards.

Only generated fixtures under this caller's resource are changed. Workers use
the production identity/configuration/artifact functions, never production keys.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
sys.dont_write_bytecode=True
from executor_lifecycle_support import (candidate_identity,configuration,digest,
    initialize_test_build_signing,require_artifact,require_identity,write_json)


def worker(request):
    value=json.loads(Path(request).read_text(encoding='utf-8'))
    root=Path(value['root']);out=root/'executor-lifecycle-repair'
    if value.get('artifact'):
        require_artifact(root,value['artifact'],value['artifact_sha256'],'fixture_artifact')
        return
    current={'candidate':candidate_identity(root/'workspace',root,out)}
    record,current['configuration']=configuration(root,out,root/'workspace')
    assert record['exit_code']==0 and not record['timed_out'],'fixture configuration probe failed'
    if 'expected' in value:
        require_identity(value['expected'],current,'cross_process')
    else:write_json(root/'snapshot.json',current)


class CrossProcessEnvironmentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root=Path(tempfile.mkdtemp(prefix='build-environment-',dir=os.environ['HIMIND_MISSION_CHECK_ROOT'])).resolve()
        cls.out=cls.root/'executor-lifecycle-repair';cls.out.mkdir()
        initialize_test_build_signing(cls.out)
        cls.workspace=cls.root/'workspace';(cls.workspace/'src').mkdir(parents=True)
        (cls.workspace/'src/main.rs').write_text('fn main() {}\n',encoding='utf-8')
        (cls.workspace/'Cargo.toml').write_text('[package]\nname="himind-agent"\nversion="0.1.0"\nedition="2021"\n',encoding='utf-8')
        (cls.workspace/'Cargo.lock').write_text('version = 3\n',encoding='utf-8')
        cls.counter=0
        cls.call()
        cls.expected=json.loads((cls.root/'snapshot.json').read_text(encoding='utf-8'))

    @classmethod
    def call(cls,changes=None,expected=False,artifact=None):
        cls.counter+=1
        value={'root':str(cls.root)}
        if expected:value['expected']=cls.expected
        if artifact:value.update(artifact=str(artifact[0]),artifact_sha256=artifact[1])
        request=cls.root/f'request-{cls.counter}.json';write_json(request,value)
        env=dict(os.environ,**(changes or {}))
        process=subprocess.run([sys.executable,'-B',__file__,'--worker',str(request)],env=env,
            capture_output=True,text=True,timeout=5)
        log=cls.root/f'worker-{cls.counter}.log'
        log.write_text(process.stdout+process.stderr,encoding='utf-8')
        if not expected and not artifact and process.returncode:
            raise AssertionError('baseline worker failed; evidence '+str(log))
        return process,log

    def rejected(self,**kwargs):
        process,log=self.call(expected=True,**kwargs)
        self.assertNotEqual(process.returncode,0,str(log))
        output=process.stdout+process.stderr
        self.assertIn('mismatch',output)
        self.assertNotIn('SENSITIVE_TEST_SENTINEL',output)
        return output

    def mutation(self,path,content):
        original=path.read_bytes()
        try:
            path.write_bytes(content)
            return self.rejected()
        finally:path.write_bytes(original)

    def test_irrelevant_parent_signing_and_process_context_normalize_across_processes(self):
        # Nonexistent synthetic paths prove inherited paths are not opened.
        for marker in ['a','b']:
            process,log=self.call(expected=True,changes={
                'HIMIND_SIGNING_KEY_ID':'SENSITIVE_TEST_SENTINEL-'+marker,
                'HIMIND_SIGNING_PRIVATE_KEY_PATH':str(self.root/('absent-private-'+marker)),
                'HIMIND_SIGNING_PUBLIC_KEY_PATH':str(self.root/('absent-public-'+marker)),
                'HIMIND_EXTENSION_SIGNING_KEY_ID':marker,
                'HIMIND_EXTENSION_SIGNING_PUBLIC_KEY_PATH':str(self.root/('absent-extension-'+marker)),
                'HIMIND_BUILD_CHANNEL':'irrelevant-parent-'+marker,'EXECUTOR_PARENT_CONTEXT':marker})
            self.assertEqual(process.returncode,0,str(log))

    def test_actual_compiler_flags_are_rejected(self):
        output=self.rejected(changes={'RUSTFLAGS':'--cfg executor_semantic_change'})
        self.assertIn('build_environment.RUSTFLAGS',output)
        self.assertIn('configuration',output)

    def test_source_and_dependency_inputs_are_rejected(self):
        for name in ['src/main.rs','Cargo.lock']:
            path=self.workspace/name
            output=self.mutation(path,path.read_bytes()+b'\n// changed fixture\n')
            self.assertIn('files.',output)

    def test_effective_cargo_configuration_is_rejected(self):
        directory=self.workspace/'.cargo';directory.mkdir(exist_ok=True)
        config=directory/'config.toml'
        config.write_text('[build]\nrustflags=["--cfg", "executor_config_changed"]\n',encoding='utf-8')
        try:self.assertIn('configuration',self.rejected())
        finally:config.unlink()

    def test_test_signing_configuration_content_is_rejected(self):
        config=self.out/'build-signing/test-build-signing.json'
        value=json.loads(config.read_text(encoding='utf-8'));value['update_key_id']='changed-test-key'
        self.assertIn('build_signing.configuration',self.mutation(config,json.dumps(value).encode()))

    def test_public_material_change_at_same_filename_is_rejected(self):
        public=self.out/'build-signing/test-public.pem'
        self.assertIn('build_signing.public_key_material',self.mutation(public,public.read_bytes()+b'\n'))

    def test_artifact_bytes_and_foreign_ownership_are_rejected(self):
        artifact=self.root/'artifact.bin';artifact.write_bytes(b'original fixture')
        expected=digest(artifact)
        good,_=self.call(artifact=(artifact,expected));self.assertEqual(good.returncode,0)
        artifact.write_bytes(b'changed fixture')
        bad,log=self.call(artifact=(artifact,expected));self.assertNotEqual(bad.returncode,0,str(log))
        self.assertIn('expected_sha256',bad.stderr)
        # The sibling is still within the authorized task resource, while
        # outside this fixture's ownership; guard must reject before hashing.
        sibling=self.root.parent/(self.root.name+'-foreign.bin');sibling.write_bytes(b'foreign fixture')
        bad,log=self.call(artifact=(sibling,expected));self.assertNotEqual(bad.returncode,0,str(log))
        self.assertIn('ownership mismatch',bad.stderr)

    def test_toolchain_selection_change_fails_closed(self):
        process,log=self.call(expected=True,changes={'RUSTUP_TOOLCHAIN':'executor-test-nonexistent-toolchain'})
        self.assertNotEqual(process.returncode,0,str(log))
        self.assertIn('rustc identity unavailable',process.stderr)


if __name__=='__main__':
    if sys.argv[1:2]==['--worker']:
        worker(sys.argv[2])
    else:unittest.main(verbosity=2)
