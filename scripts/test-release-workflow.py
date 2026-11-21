"""Exercise the exact privileged workflow code with inert APIs and ZIP fixtures."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import zipfile


WORKFLOW = (Path(__file__).resolve().parents[1] / '.github/workflows/release.yml').read_text()
SHA = 'a' * 40
VERSION = '1.8.4'
PACKAGE = f'BetterTrafficMonitorAiUsage_v{VERSION}_x64.zip'
PREFIX = 'artifacts/native-build/'


def inline_blocks(marker, indentation):
    blocks = []
    lines = WORKFLOW.splitlines()
    for index, line in enumerate(lines):
        if line == marker:
            block = []
            for following in lines[index + 1:]:
                if not following:
                    block.append('')
                elif following.startswith(' ' * indentation):
                    block.append(following[indentation:])
                else:
                    break
            assert ''.join(block).strip(), f'Empty inline block: {marker}'
            blocks.append('\n'.join(block))
    return blocks


scripts = inline_blocks('          script: |', 12)
validators = inline_blocks('        run: |', 10)
assert len(scripts) == 3 and len(validators) == 1, 'Workflow layout changed: update extraction explicitly'
assert 'actions/checkout@' not in WORKFLOW and 'build-release.ps1' not in WORKFLOW
assert 'workflows: [CI, quality-gate, CodeQL]' in WORKFLOW
assert "github.event.workflow_run.head_repository.id == github.event.repository.id" in WORKFLOW

NODE_TESTS = r'''
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const sources = JSON.parse(require('node:fs').readFileSync(0, 'utf8'));
const run = source => new Function('github', 'context', 'core', 'require', 'process',
  `return (async () => {${source}\n})();`);
const execute = sources.map(run);
const sha = 'a'.repeat(40);
const workflows = ['ci.yml', 'quality-gate.yml', 'codeql.yml'];
const absent = async () => { throw Object.assign(new Error('absent'), { status: 404 }); };
let passed = 0;
function fixture() {
  const context = { repo: { owner: 'qqrm', repo: 'better-trafficmonitor-ai-usage-plugin' },
    payload: { repository: { id: 77 }, workflow_run: { event: 'push', head_branch: 'main',
      head_sha: sha, head_repository: { id: 77 }, conclusion: 'success' } } };
  const runs = Object.fromEntries(workflows.map((workflow, index) => [workflow, [{
    id: 100 + index, head_sha: sha, event: 'push', head_branch: 'main', head_repository: { id: 77 },
    status: 'completed', conclusion: 'success',
  }]]));
  const artifacts = [{ id: 200, name: 'BetterTrafficMonitorAiUsage-x64', expired: false,
    digest: 'sha256:' + 'b'.repeat(64), workflow_run: { id: 100, head_sha: sha, head_repository_id: 77 } }];
  const outputs = {}, mutations = [];
  const actions = { listWorkflowRuns() {}, listWorkflowRunArtifacts() {} };
  const github = { rest: { actions, git: { getRef: absent }, repos: {
    getReleaseByTag: absent,
    createRelease: async args => { mutations.push(['draft', args]); return { data: { id: 500 } }; },
    uploadReleaseAsset: async args => { mutations.push(['asset', args]); return { data: {} }; },
    updateRelease: async args => { mutations.push(['publish', args]); return { data: {} }; },
  } }, paginate: async (method, args) => method === actions.listWorkflowRuns ?
    structuredClone(runs[args.workflow_id]) : structuredClone(artifacts) };
  const core = { setOutput: (name, value) => { outputs[name] = value; }, notice() {} };
  const env = { SOURCE_SHA: sha, RELEASE_VERSION: '1.8.4', RUNNER_TEMP: '/unused',
    GATE_RUNS: JSON.stringify(Object.fromEntries(workflows.map((workflow, index) => [workflow, 100 + index]))) };
  const mockRequire = name => name === 'node:fs' ? { readFileSync: () => Buffer.from('verified asset') } : require(name);
  return { context, runs, artifacts, outputs, mutations, github, core, env, mockRequire };
}
async function gate(f) { await execute[0](f.github, f.context, f.core, require, { env: f.env }); }
async function publish(f) { await execute[2](f.github, f.context, f.core, f.mockRequire, { env: f.env }); }
(async () => {
  const valid = fixture(); await gate(valid); assert.equal(valid.outputs.ready, 'true'); passed++;
  for (const [name, change] of [
    ['pull request trigger', f => { f.context.payload.workflow_run.event = 'pull_request'; }],
    ['fork trigger', f => { f.context.payload.workflow_run.head_repository.id = 99; }],
    ['non-main trigger', f => { f.context.payload.workflow_run.head_branch = 'feature'; }],
    ['failed trigger', f => { f.context.payload.workflow_run.conclusion = 'failure'; }],
    ['expired artifact', f => { f.artifacts[0].expired = true; }],
    ['ambiguous artifact', f => { f.artifacts.push(structuredClone(f.artifacts[0])); }],
    ['wrong artifact run', f => { f.artifacts[0].workflow_run.id = 101; }],
    ['wrong artifact source', f => { f.artifacts[0].workflow_run.head_sha = 'c'.repeat(40); }],
    ['fork artifact', f => { f.artifacts[0].workflow_run.head_repository_id = 99; }],
    ['missing artifact digest', f => { delete f.artifacts[0].digest; }],
  ]) {
    const f = fixture(); change(f); await assert.rejects(gate(f), undefined, name);
    assert.notEqual(f.outputs.ready, 'true', name); passed++;
  }
  for (const [name, change] of [
    ['missing workflow', f => { f.runs['codeql.yml'] = []; }],
    ['pending workflow', f => { f.runs['ci.yml'][0].status = 'in_progress'; }],
    ['failed quality gate', f => { f.runs['quality-gate.yml'][0].conclusion = 'failure'; }],
    ['success for another commit', f => { f.runs['ci.yml'][0].head_sha = 'b'.repeat(40); }],
    ['pull-request success only', f => { f.runs['ci.yml'][0].event = 'pull_request'; }],
    ['newer failed run', f => { f.runs['ci.yml'].push({ ...f.runs['ci.yml'][0], id: 999, conclusion: 'failure' }); }],
  ]) {
    const f = fixture(); change(f); await gate(f); assert.equal(f.outputs.ready, 'false', name); passed++;
  }
  for (const [name, change] of [
    ['gate failed before publish', f => { f.runs['codeql.yml'][0].conclusion = 'failure'; }],
    ['gate rerun pending', f => { f.runs['ci.yml'][0].status = 'queued'; }],
    ['newer CI run', f => { f.runs['ci.yml'].push({ ...f.runs['ci.yml'][0], id: 999 }); }],
    ['different tag target', f => { f.github.rest.git.getRef = async () => ({ data: { object: { type: 'commit', sha: 'b'.repeat(40) } } }); }],
    ['existing draft', f => { f.github.rest.repos.getReleaseByTag = async () => ({ data: { draft: true } }); }],
  ]) {
    const f = fixture(); change(f); await assert.rejects(publish(f), undefined, name);
    assert.equal(f.mutations.length, 0, name); passed++;
  }
  const existing = fixture();
  existing.github.rest.git.getRef = async () => ({ data: { object: { type: 'commit', sha } } });
  existing.github.rest.repos.getReleaseByTag = async () => ({ data: { draft: false } });
  await publish(existing); assert.equal(existing.mutations.length, 0); passed++;
  for (const status of ['ahead', 'diverged']) {
    const f = fixture();
    f.github.rest.git.getRef = async () => ({ data: { object: { type: 'commit', sha: 'b'.repeat(40) } } });
    f.github.rest.repos.getReleaseByTag = async () => ({ data: { draft: false } });
    f.github.rest.repos.compareCommits = async args => {
      assert.equal(args.base, 'b'.repeat(40)); assert.equal(args.head, sha);
      return { data: { status } };
    };
    if (status === 'ahead') await publish(f);
    else await assert.rejects(publish(f), /not an ancestor/);
    assert.equal(f.mutations.length, 0); passed++;
  }
  const successful = fixture(); await publish(successful);
  assert.equal(successful.mutations[0][0], 'draft');
  assert.equal(successful.mutations[0][1].draft, true);
  assert.equal(successful.mutations[0][1].target_commitish, sha);
  assert.equal(successful.mutations.filter(([action]) => action === 'asset').length, 5);
  assert.equal(successful.mutations.at(-1)[0], 'publish'); passed++;
  const interrupted = fixture();
  interrupted.github.rest.repos.uploadReleaseAsset = async () => { throw new Error('upload unavailable'); };
  await assert.rejects(publish(interrupted), /upload unavailable/);
  assert.equal(interrupted.mutations.filter(([action]) => action === 'publish').length, 0); passed++;
  const changedDuringUpload = fixture();
  changedDuringUpload.github.rest.repos.uploadReleaseAsset = async args => {
    changedDuringUpload.mutations.push(['asset', args]);
    changedDuringUpload.runs['quality-gate.yml'][0].status = 'in_progress';
    return { data: {} };
  };
  await assert.rejects(publish(changedDuringUpload), /Gate changed before publication/);
  assert.equal(changedDuringUpload.mutations.filter(([action]) => action === 'asset').length, 5);
  assert.equal(changedDuringUpload.mutations.filter(([action]) => action === 'publish').length, 0); passed++;
  const data = Buffer.from('immutable fixture bytes');
  const digest = 'sha256:' + crypto.createHash('sha256').update(data).digest('hex');
  for (const correctDigest of [true, false]) {
    let writes = 0;
    const requireMock = name => name === 'node:fs' ? { writeFileSync: (_, bytes) => {
      assert.deepEqual(bytes, data); writes++;
    } } : require(name);
    const promise = execute[1]({ rest: { actions: { downloadArtifact: async () => ({ data }) } } },
      fixture().context, {}, requireMock, { env: { RUNNER_TEMP: '/unused', ARTIFACT_ID: '200',
        ARTIFACT_DIGEST: correctDigest ? digest : 'sha256:' + '0'.repeat(64) } });
    if (correctDigest) { await promise; assert.equal(writes, 1); }
    else { await assert.rejects(promise, /digest differs/); assert.equal(writes, 0); }
    passed++;
  }
  console.log(`PASS ${passed} release gate/publication cases (inert GitHub API)`);
})().catch(error => { console.error(error); process.exitCode = 1; });
'''


def zip_bytes(files):
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w') as archive:
        for name, data in files.items():
            archive.writestr(name, data)
    return output.getvalue()


def fixture():
    dll = b'inert DLL fixture: never executed'
    package = zip_bytes({'plugins/BetterTrafficMonitorAiUsage.dll': dll})
    build = {'sourceSha': SHA, 'version': VERSION, 'dllSha256': hashlib.sha256(dll).hexdigest(),
             'zipSha256': hashlib.sha256(package).hexdigest(), 'nativeAnalysisRequested': True,
             'nativeCompilerLog': 'msbuild.log'}
    smoke = {key: build[key] for key in ['sourceSha', 'dllSha256', 'zipSha256']}
    smoke.update(status='passed', actualTrafficMonitor=False, externalProviderPolling=False,
                 checks=['factory', 'API', 'version', 'index bounds', 'provider IDs',
                         'initialization', 'cached text', 'GDI drawing', 'visibility configuration'])
    return build, smoke, package


def validate_case(name, mutate=None, expected_error=None, extra=None):
    build, smoke, package = fixture()
    if mutate:
        mutate(build, smoke)
    files = {PREFIX + 'build.json': json.dumps(build).encode(),
             PREFIX + 'dll-smoke.json': json.dumps(smoke).encode(),
             PREFIX + 'msbuild.log': b'fixture compiler log', 'dist/' + PACKAGE: package,
             'dist/' + PACKAGE + '.sha256': f'{hashlib.sha256(package).hexdigest()}  {PACKAGE}\r\n'.encode()}
    if extra:
        extra(files)
    with tempfile.TemporaryDirectory(prefix='ai-release-fixture-') as directory:
        root = Path(directory)
        (root / 'ci-artifact.zip').write_bytes(zip_bytes(files))
        result = subprocess.run([sys.executable, '-c', validators[0]], text=True, capture_output=True,
                                env={**os.environ, 'RUNNER_TEMP': directory, 'SOURCE_SHA': SHA,
                                     'GITHUB_OUTPUT': str(root / 'outputs')}, check=False)
        if expected_error:
            assert result.returncode != 0 and expected_error in result.stderr, (name, result.stderr)
            assert not (root / 'verified-release').exists(), name
        else:
            assert result.returncode == 0, (name, result.stderr)
            expected = {PACKAGE, PACKAGE + '.sha256', 'build.json', 'dll-smoke.json', 'msbuild.log'}
            assert {item.name for item in (root / 'verified-release').iterdir()} == expected
            assert (root / 'verified-release' / PACKAGE).read_bytes() == package
            assert (root / 'outputs').read_text() == f'version={VERSION}\n'
    print(f'PASS artifact validation: {name}')


def main():
    subprocess.run(['node', '-e', NODE_TESTS], input=json.dumps(scripts), text=True, check=True)
    validate_case('valid package, evidence, and exact bytes')
    for name, mutate, expected in [
        ('wrong build source', lambda b, s: b.update(sourceSha='b' * 40), 'another source commit'),
        ('wrong smoke source', lambda b, s: s.update(sourceSha='b' * 40), 'another source commit'),
        ('analysis disabled', lambda b, s: b.update(nativeAnalysisRequested=False), 'analysis evidence missing'),
        ('analysis string is not boolean', lambda b, s: b.update(nativeAnalysisRequested='true'), 'analysis evidence missing'),
        ('failed smoke', lambda b, s: s.update(status='failed'), 'smoke evidence missing'),
        ('missing draw check', lambda b, s: s['checks'].remove('GDI drawing'), 'Incomplete DLL smoke'),
        ('wrong package hash', lambda b, s: b.update(zipSha256='0' * 64), 'Package hash differs'),
        ('wrong DLL hash', lambda b, s: s.update(dllSha256='0' * 64), 'DLL hash differs'),
        ('version path injection', lambda b, s: b.update(version='../escape'), 'Invalid release version'),
    ]:
        validate_case(name, mutate, expected)
    validate_case('missing smoke file', expected_error='dll-smoke.json',
                  extra=lambda files: files.pop(PREFIX + 'dll-smoke.json'))
    validate_case('wrong checksum', expected_error='checksum file is inconsistent',
                  extra=lambda files: files.update({'dist/' + PACKAGE + '.sha256': b'wrong checksum'}))
    validate_case('path traversal entry', expected_error='Unsafe ZIP entry',
                  extra=lambda files: files.update({'../escape': b'never extracted'}))
    validate_case('Windows path traversal entry', expected_error='Unsafe ZIP entry',
                  extra=lambda files: files.update({'..\\escape': b'never extracted'}))
    validate_case('unrelated artifact data stays unpublished',
                  extra=lambda files: files.update({'untrusted-helper.py': b'raise RuntimeError("never executed")'}))


if __name__ == '__main__':
    main()
