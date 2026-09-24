"""Gate logic tests use synthetic records; these are not release acceptance evidence."""
import hashlib, importlib.util, io, json, pathlib, tarfile, tempfile, unittest
spec=importlib.util.spec_from_file_location('release_gates',pathlib.Path(__file__).parents[1]/'scripts/release-gates.py')
gates=importlib.util.module_from_spec(spec);spec.loader.exec_module(gates)
class ReleaseGates(unittest.TestCase):
    def test_github_workflow_path_accepts_documented_ref_suffix(self):
        run={'path':'.github/workflows/ci.yml@main','head_branch':'main','head_sha':'a'*40}
        self.assertTrue(gates.workflow_path_matches(run,'.github/workflows/ci.yml'))
        self.assertFalse(gates.workflow_path_matches({**run,'path':'.github/workflows/ci.yml@other'},'.github/workflows/ci.yml'))
        self.assertTrue(gates.workflow_path_matches({**run,'path':'.github/workflows/ci.yml'},'.github/workflows/ci.yml'))
    def test_archive_identity_checks_internal_target_and_commit(self):
        version='1.0.0-rc.1';target='x86_64-unknown-linux-gnu';commit='a'*40
        root=f'casimir-{version}-{target}/'
        header=b'\x7fELF\x02\x01'+b'\0'*12+b'\x3e\x00'+b'\0'*44
        info={'schemaVersion':1,'version':version,'target':target,'commit':commit,'workflowRun':'123'}
        with tempfile.TemporaryDirectory() as temporary:
            path=pathlib.Path(temporary)/'archive.tar.gz'
            with tarfile.open(path,'w:gz') as archive:
                for name,contents in [('build-info.json',json.dumps(info).encode()),('casimir',header)]:
                    item=tarfile.TarInfo(root+name);item.size=len(contents)
                    archive.addfile(item,io.BytesIO(contents))
            self.assertTrue(gates.archive_identity(path,version,target,commit,'123'))
            self.assertFalse(gates.archive_identity(path,version,target,'b'*40,'123'))
            self.assertFalse(gates.archive_identity(path,version,target,commit,'456'))
            self.assertFalse(gates.native_header_matches(header,'x86_64-pc-windows-msvc'))
    def reliability(self, root, commit='fixture-commit'):
        records={}
        for platform, runner in [('linux','ubuntu-latest'),('macos','macos-latest'),('windows','windows-latest')]:
            name=f'reliability-{platform}.json'
            data={'schemaVersion':1,'redacted':True,'status':'passed','kind':'reliability','platform':platform,'commit':commit,
                  'source':'GitHub Actions CI push workflow','runId':'123','event':'push',
                  'jobs':[{'name':f'test ({runner}, {rust})','jobId':index,'conclusion':'success'}
                          for index,rust in enumerate(['1.85.0','stable'],1)],
                  'checks':['cargo test --locked --all-targets',"python -m unittest discover -s tests -p test_*.py"]}
            path=root/name;path.write_text(json.dumps(data))
            records[platform]={'status':'passed','artifact':name,'sha256':hashlib.sha256(path.read_bytes()).hexdigest()}
        return records
    def test_missing_evidence_and_wrong_commit_block_release(self):
        self.assertTrue(gates.validate({},'commit','1.0',pathlib.Path('.')))
    def test_rc_requires_all_platform_artifacts_and_does_not_certify_one_point_zero(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=pathlib.Path(temporary)
            evidence={'schemaVersion':1,'redacted':True,'commit':'fixture-commit','reliability':self.reliability(root)}
            self.assertEqual(gates.validate(evidence,'fixture-commit','rc',root),[])
            self.assertTrue(gates.validate(evidence,'fixture-commit','1.0',root))
            (root/'reliability-linux.json').write_text('changed')
            self.assertTrue(gates.validate(evidence,'fixture-commit','rc',root))
    def test_rejects_one_fabricated_file_for_all_reliability_platforms(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=pathlib.Path(temporary);artifact=root/'fake.json'
            artifact.write_text(json.dumps({'schemaVersion':1,'redacted':True,'status':'passed','commit':'fixture-commit'}))
            record={'status':'passed','artifact':'fake.json','sha256':hashlib.sha256(artifact.read_bytes()).hexdigest()}
            evidence={'schemaVersion':1,'redacted':True,'commit':'fixture-commit','reliability':{p:record for p in ['linux','macos','windows']}}
            self.assertTrue(gates.validate(evidence,'fixture-commit','rc',root))
    def test_rejects_wrong_platform_even_with_matching_hash(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=pathlib.Path(temporary);records=self.reliability(root)
            path=root/'reliability-windows.json';data=json.loads(path.read_text());data['platform']='linux';path.write_text(json.dumps(data))
            records['windows']['sha256']=hashlib.sha256(path.read_bytes()).hexdigest()
            evidence={'schemaVersion':1,'redacted':True,'commit':'fixture-commit','reliability':records}
            self.assertTrue(gates.validate(evidence,'fixture-commit','rc',root))
    def test_staging_requires_redaction_and_uploads_only_referenced_json(self):
        specification=importlib.util.spec_from_file_location('prepare_evidence',pathlib.Path(__file__).parents[1]/'scripts/prepare-evidence.py')
        staging=importlib.util.module_from_spec(specification);specification.loader.exec_module(staging)
        with tempfile.TemporaryDirectory() as temporary:
            root=pathlib.Path(temporary);source=root/'source';source.mkdir()
            records=self.reliability(source)
            (source/'private-transcript.txt').write_text('must never be uploaded')
            evidence={'schemaVersion':1,'redacted':True,'commit':'fixture-commit','reliability':records}
            (source/'release-evidence.json').write_text(json.dumps(evidence))
            staging.prepare(source,root/'staged','fixture-commit','rc')
            self.assertEqual({p.name for p in (root/'staged').iterdir()},{'release-evidence.json',*(f'reliability-{p}.json' for p in ['linux','macos','windows'])})
            artifact=source/'reliability-linux.json'
            artifact.write_text(json.dumps({'status':'passed'}));records['linux']['sha256']=hashlib.sha256(artifact.read_bytes()).hexdigest()
            (source/'release-evidence.json').write_text(json.dumps(evidence))
            with self.assertRaisesRegex(ValueError,'redaction'):
                staging.prepare(source,root/'refused','fixture-commit','rc')
if __name__=='__main__':unittest.main()
