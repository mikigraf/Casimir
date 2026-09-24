"""Gate logic tests use synthetic records; these are not release acceptance evidence."""
import hashlib, importlib.util, json, pathlib, tempfile, unittest
spec=importlib.util.spec_from_file_location('release_gates',pathlib.Path(__file__).parents[1]/'scripts/release-gates.py')
gates=importlib.util.module_from_spec(spec);spec.loader.exec_module(gates)
class ReleaseGates(unittest.TestCase):
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
