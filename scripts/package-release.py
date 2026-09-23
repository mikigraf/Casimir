#!/usr/bin/env python3
"""Build archives from already-built native executables and verify extracted installation."""
import argparse, hashlib, json, os, pathlib, shutil, subprocess, tarfile, tempfile, zipfile

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--target',required=True);parser.add_argument('--version',required=True);parser.add_argument('--output',type=pathlib.Path,default=pathlib.Path('dist'))
    args=parser.parse_args()
    import re
    if not re.fullmatch(r'1\.0\.0(?:-rc\.[1-9][0-9]*)?',args.version):raise SystemExit('expected 1.0.0 or 1.0.0-rc.N')
    if args.target not in ['aarch64-apple-darwin','x86_64-apple-darwin','x86_64-unknown-linux-gnu','x86_64-pc-windows-msvc']:raise SystemExit('unsupported release target')
    extension='.exe' if 'windows' in args.target else ''
    executable=pathlib.Path('target')/args.target/'release'/('casimir'+extension)
    version=subprocess.check_output([str(executable),'--version'],text=True).strip()
    if version!='casimir '+args.version:raise SystemExit('executable version does not match archive version')
    args.output.mkdir(parents=True,exist_ok=True)
    name='casimir-'+args.version+'-'+args.target
    with tempfile.TemporaryDirectory() as temporary:
        directory=pathlib.Path(temporary)/name;directory.mkdir()
        shutil.copy2(executable,directory/executable.name)
        for path in ['README.md','LICENSE','CHANGELOG.md','SECURITY.md']:shutil.copy2(path,directory/path)
        shutil.copytree('docs',directory/'docs');shutil.copytree('compatibility',directory/'compatibility')
        provenance={'schemaVersion':1,'version':args.version,'target':args.target,'commit':subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),'rustc':subprocess.check_output(['rustc','--version'],text=True).strip(),'cargoLockSha256':hashlib.sha256(pathlib.Path('Cargo.lock').read_bytes()).hexdigest(),'workflowRun':os.environ.get('GITHUB_RUN_ID')}
        (directory/'build-info.json').write_text(json.dumps(provenance,indent=2)+'\n')
        archive=args.output/(name+('.zip' if extension else '.tar.gz'))
        if extension:
            with zipfile.ZipFile(archive,'w',zipfile.ZIP_DEFLATED) as output:
                for path in sorted(directory.rglob('*')):
                    if path.is_file():output.write(path,path.relative_to(directory.parent))
        else:
            with tarfile.open(archive,'w:gz') as output:output.add(directory,arcname=name)
        # Extract the actual archive into a clean location and run that binary.
        install=pathlib.Path(temporary)/'install';install.mkdir()
        if extension:
            with zipfile.ZipFile(archive) as source:source.extractall(install)
        else:
            with tarfile.open(archive) as source:source.extractall(install,filter='data')
        installed=install/name/executable.name
        assert subprocess.check_output([str(installed),'--version'],text=True).strip()==version
        doctor=json.loads(subprocess.check_output([str(installed),'doctor','--json'],text=True))
        assert doctor['paidCalls'] is False
        checksum=hashlib.sha256(archive.read_bytes()).hexdigest()
        archive.with_suffix(archive.suffix+'.sha256').write_text(checksum+'  '+archive.name+'\n')
        (args.output/(name+'.installation.json')).write_text(json.dumps({'schemaVersion':1,'status':'passed','archive':archive.name,'sha256':checksum,'version':version,'paidCalls':False},indent=2)+'\n')
if __name__=='__main__':main()
