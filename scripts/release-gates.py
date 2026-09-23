#!/usr/bin/env python3
"""Fail closed unless release evidence matches this commit and the agreed contract."""
import argparse, hashlib, json, pathlib, re, sys

def validate(evidence, commit, stage, base):
    blockers=[]
    def require(condition, message):
        if not condition: blockers.append(message)
    require(evidence.get('schemaVersion')==1, 'unsupported evidence schema')
    require(evidence.get('commit')==commit, 'evidence does not match the release commit')
    def passed(record, label):
        if not isinstance(record,dict): blockers.append(label+': evidence missing'); return
        require(record.get('status')=='passed',label+': not passed')
        artifact=record.get('artifact'); digest=record.get('sha256')
        if not artifact or not digest: blockers.append(label+': artifact/hash missing'); return
        path=(base/artifact).resolve()
        require(path.is_relative_to(base.resolve()),label+': artifact escapes evidence directory')
        if not path.is_file(): blockers.append(label+': artifact unavailable'); return
        require(hashlib.sha256(path.read_bytes()).hexdigest()==digest,label+': artifact hash mismatch')
    for platform in ['linux','macos','windows']:
        passed(evidence.get('reliability',{}).get(platform),'reliability '+platform)
    if stage=='1.0':
        live=evidence.get('live',{})
        for platform in ['linux','macos','windows']: passed(live.get(platform),'authenticated '+platform)
        require(live.get('linuxTaskCount',0)>=10,'ten maintained Linux tasks required')
        require(live.get('linuxReplicatesPerHarnessPerTask',0)>=2,'two replicates for both harnesses required')
        require(set(live.get('harnesses',[]))=={'claude-code','codex'},'both supported harnesses required')
        require(set(live.get('replayDirections',[]))=={'claude-code->codex','codex->claude-code'},'both replay directions required')
        require(all(live.get('workflows',{}).get(p)==['replay','resume','checkpoint'] for p in ['macos','windows']),'native replay/resume/checkpoint evidence required')
        passed(evidence.get('evaluation'),'reviewed evaluation')
        calibration=evidence.get('calibration') or {}
        require(calibration.get('cases')==40 and calibration.get('passed') is True,'40 reviewed trace pairs must pass calibration')
        require(calibration.get('agreement',0)>=0.90 and calibration.get('requiredCheckViolations')==0,'calibration thresholds not met')
        require(len(set(calibration.get('reviewers',[])[:2]))==2,'independent reviewers missing')
        for name in ['simulator','attribution']: passed(evidence.get(name),name+' reviewed cases')
        pilots=evidence.get('pilots',[])
        require(len(pilots)>=3 and len({p.get('userId') for p in pilots if p.get('userId')})>=3,'three independent pilot users required')
        for pilot in pilots:
            passed(pilot,'pilot '+str(pilot.get('userId')))
            require(all(pilot.get('steps',{}).get(s)=='passed' for s in ['install','doctor','experiment','interpret','recover','cleanup']),'pilot journey incomplete')
        require(evidence.get('unresolvedBlockers')==[],'unresolved data-loss, incorrect-result or onboarding blockers')
        passed(evidence.get('releaseCandidate'),'release candidate')
    return blockers

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--evidence',required=True,type=pathlib.Path);parser.add_argument('--commit',required=True);parser.add_argument('--stage',choices=['rc','1.0'],required=True)
    args=parser.parse_args()
    try: evidence=json.loads(args.evidence.read_text()); blockers=validate(evidence,args.commit,args.stage,args.evidence.parent)
    except (OSError,ValueError,TypeError,KeyError) as error: blockers=['invalid or unavailable evidence: '+str(error)]
    print(json.dumps({'schemaVersion':1,'commit':args.commit,'stage':args.stage,'passed':not blockers,'blockers':blockers},indent=2))
    return bool(blockers)
if __name__=='__main__':sys.exit(main())
