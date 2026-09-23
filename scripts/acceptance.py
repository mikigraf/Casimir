#!/usr/bin/env python3
"""Opt-in authenticated orchestration gate. Model failures are valid experimental outcomes."""
import argparse, hashlib, json, os, pathlib, platform, subprocess, sys

def command(args, cwd=None, output=None):
    if output is None:return subprocess.run(args,cwd=cwd,check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    with output.open('wb') as stream:return subprocess.run(args,cwd=cwd,stdout=stream,stderr=subprocess.STDOUT)

def write(path, value):path.write_text(json.dumps(value,indent=2)+'\n')
def main():
    parser=argparse.ArgumentParser();parser.add_argument('--casimir',type=pathlib.Path,required=True);parser.add_argument('--output',type=pathlib.Path,required=True)
    parser.add_argument('--tasks',type=pathlib.Path,default=pathlib.Path('acceptance/live/tasks.json'));parser.add_argument('--allow-paid',action='store_true');parser.add_argument('--allow-unrestricted',action='store_true')
    args=parser.parse_args()
    if not args.allow_paid:parser.error('authenticated acceptance requires explicit --allow-paid')
    binary=str(args.casimir.resolve());out=args.output.resolve()
    if out.exists() and any(out.iterdir()):parser.error('output must be empty')
    out.mkdir(parents=True,exist_ok=True)
    doctor=json.loads(command([binary,'doctor','--json']).stdout);write(out/'doctor.json',doctor)
    missing=[h['id'] for h in doctor['harnesses'] if h['id'] in ['claude-code','codex'] and h['authentication']!='authenticated']
    if missing:
        write(out/'acceptance.json',{'schemaVersion':1,'status':'blocked','missingAuthentication':missing,'fixtureSubstitution':False});return 1
    tasks=json.loads(args.tasks.read_text())['tasks']
    if len(tasks)!=10:raise SystemExit('expected ten maintained tasks')
    records=[];saved={};issues=[]
    for task in tasks:
        root=out/task['id'];root.mkdir();repo=root/'source';repo.mkdir()
        for name,content in task['files'].items():
            path=repo/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(content)
        for git_args in [['init','-q'],['add','.'],['-c','user.name=acceptance','-c','user.email=acceptance@example.invalid','commit','-qm','task baseline']]:command(['git',*git_args],repo)
        session={'schemaVersion':1,'id':task['id'],'harness':'claude-code','cwd':str(repo),'gitCommit':command(['git','rev-parse','HEAD'],repo).stdout.strip(),'events':[{'turn':i+1,'ts':'','kind':'user','text':prompt} for i,prompt in enumerate(task['prompts'])]}
        write(root/'input.json',session);checker=root/'checker.py';checker.write_text(task['checker'])
        checks=root/'checks.json';write(checks,{'schemaVersion':1,'checks':[{'executable':sys.executable,'args':[str(checker)],'timeoutSecs':60,'expectedExitStatus':0}]})
        checker_hash=hashlib.sha256(checker.read_bytes()).hexdigest()
        for harness in ['claude-code','codex']:
            for replicate in [1,2]:
                run=root/(harness+'-'+str(replicate));argv=[binary,'rerun',str(root/'input.json'),'--harness',harness,'--workspace',str(repo),'--replicates','1','--checks',str(checks),'-o',str(run),'--quiet']
                if args.allow_unrestricted:argv+=['--allow-unrestricted',*(['--permission-mode','bypassPermissions'] if harness=='claude-code' else ['--sandbox','danger-full-access'])]
                result=command(argv,output=root/(harness+'-'+str(replicate)+'.log'))
                report=json.loads((run/'report.json').read_text()) if (run/'report.json').exists() else {}
                valid=(result.returncode==0 and report.get('executionStatus')=='completed' and report.get('checks',{}).get('outcome') in ['passed','failed'] and not (report.get('checks',{}).get('outcome')=='failed' and report.get('overallOutcome')=='passed'))
                if command(['git','status','--porcelain'],repo).stdout.strip() or hashlib.sha256(checker.read_bytes()).hexdigest()!=checker_hash:valid=False
                record={'task':task['id'],'harness':harness,'replicate':replicate,'orchestration':'passed' if valid else 'failed','taskOutcome':report.get('overallOutcome','inconclusive'),'run':str(run)};records.append(record)
                if not valid:issues.append(record)
                if replicate==1 and valid:saved[(task['id'],harness)]=(run,repo,checks)
                write(out/'progress.json',{'schemaVersion':1,'records':records,'issues':issues})
    directions=[]
    for source,target in [('claude-code','codex'),('codex','claude-code')]:
        candidate=saved.get((tasks[0]['id'],source))
        if candidate:
            source_run,repo,checks=candidate;run=out/('cross-'+source+'-'+target)
            result=command([binary,'rerun',str(source_run),'--harness',target,'--workspace',str(repo),'--replicates','1','--checks',str(checks),'-o',str(run),'--quiet'],output=out/('cross-'+source+'-'+target+'.log'))
            if result.returncode==0:directions.append(source+'->'+target)
    summary={'schemaVersion':1,'commit':command(['git','rev-parse','HEAD']).stdout.strip(),'platform':platform.system().lower(),'status':'passed' if not issues and len(directions)==2 else 'failed','linuxTaskCount':len(tasks),'linuxReplicatesPerHarnessPerTask':2,'harnesses':['claude-code','codex'],'replayDirections':directions,'records':records,'issues':issues,'nativeCheckpointResumeGate':'pending separate authenticated workflow evidence','modelFailuresAllowed':True,'fixtureSubstitution':False}
    write(out/'acceptance.json',summary);return int(summary['status']!='passed')
if __name__=='__main__':sys.exit(main())
