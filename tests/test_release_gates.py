"""Gate logic tests use synthetic records; these are not release acceptance evidence."""
import hashlib, importlib.util, pathlib, tempfile, unittest
spec=importlib.util.spec_from_file_location('release_gates',pathlib.Path(__file__).parents[1]/'scripts/release-gates.py')
gates=importlib.util.module_from_spec(spec);spec.loader.exec_module(gates)
class ReleaseGates(unittest.TestCase):
    def test_missing_evidence_and_wrong_commit_block_release(self):
        self.assertTrue(gates.validate({},'commit','1.0',pathlib.Path('.')))
    def test_rc_requires_all_platform_artifacts_and_does_not_certify_one_point_zero(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=pathlib.Path(temporary);artifact=root/'fixture.txt';artifact.write_text('synthetic test evidence')
            record={'status':'passed','artifact':'fixture.txt','sha256':hashlib.sha256(artifact.read_bytes()).hexdigest()}
            evidence={'schemaVersion':1,'commit':'fixture-commit','reliability':{p:record for p in ['linux','macos','windows']}}
            self.assertEqual(gates.validate(evidence,'fixture-commit','rc',root),[])
            self.assertTrue(gates.validate(evidence,'fixture-commit','1.0',root))
            artifact.write_text('changed')
            self.assertTrue(gates.validate(evidence,'fixture-commit','rc',root))
if __name__=='__main__':unittest.main()
