'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { BridgeClient } = require('../../src/bridge-client');
const { ComparisonService } = require('../../src/comparison-service');
const { ComparisonModel } = require('../../src/comparison-model');
const { isolatedGitEnv } = require('./git-env');

async function main() {
  const executable = process.env.GITPANE_TEST_BINARY;
  if (!executable || !path.isAbsolute(executable) || !fs.existsSync(executable)) {
    throw new Error('Set GITPANE_TEST_BINARY to the built Gitpane binary.');
  }
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-compare-real-'));
  const git = (...args) => {
    const result = spawnSync('git', ['-C', directory, '-c', 'user.name=Gitpane Test',
      '-c', 'user.email=gitpane-test@example.invalid', ...args], { env: isolatedGitEnv(), encoding: 'utf8' });
    if (result.error || result.status !== 0) throw new Error(result.error?.message || result.stderr);
    return result.stdout.trim();
  };
  try {
    git('init', '-q');
    git('branch', '-M', 'main');
    fs.writeFileSync(path.join(directory, 'old.txt'), 'unchanged content\n');
    git('add', 'old.txt'); git('commit', '-qm', 'base');
    const base = git('rev-parse', 'HEAD');
    git('checkout', '-qb', 'feature');
    git('mv', 'old.txt', 'new.txt'); git('commit', '-qm', 'rename');
    const feature = git('rev-parse', 'HEAD');
    git('checkout', '-q', 'main');
    fs.writeFileSync(path.join(directory, 'main.txt'), 'main\n');
    git('add', 'main.txt'); git('commit', '-qm', 'main');
    const mainHead = git('rev-parse', 'HEAD');
    fs.writeFileSync(path.join(directory, 'main.txt'), 'staged and dirty\n');
    git('add', 'main.txt');
    fs.writeFileSync(path.join(directory, 'untracked.txt'), 'local\n');
    const before = git('status', '--porcelain=v1');
    const snapshot = { executable, roots: [directory], generation: 1 };
    const backend = { generation: 1, vscode: { workspace: { isTrusted: true } }, externalSelections: new Map(),
      mutationSnapshot: async () => snapshot, isCurrentMutation: (_scope, repo) => repo === directory,
      createClient: (binary, options) => new BridgeClient(binary, options) };
    const model = new ComparisonModel(new ComparisonService(backend), directory, () => true);
    const refs = await model.service.request('revision_refs', directory, { offset: 0 });
    assert.ok(refs.refs.some(ref => ref.name === 'refs/heads/main' && ref.oid === mainHead));
    assert.ok(refs.refs.some(ref => ref.name === 'refs/heads/feature' && ref.oid === feature));
    await model.set('from', 'refs/heads/main');
    await model.set('to', 'refs/heads/feature');
    model.setMode('common_ancestor');
    const comparison = await model.compare();
    assert.equal(comparison.base_oid, base);
    assert.equal(comparison.to_oid, feature);
    assert.ok(comparison.files.some(file => file.status === 'R' && file.old_path === 'old.txt' && file.path === 'new.txt'));
    assert.equal(git('status', '--porcelain=v1'), before);
    git('update-ref', 'refs/heads/feature', mainHead);
    await model.checkRefs();
    assert.equal(model.to.moved, true);
    assert.equal(model.to.oid, feature);
    assert.equal((await model.compare()).to_oid, feature);
    await model.refresh();
    assert.equal((await model.compare()).to_oid, mainHead);
    model.dispose();
    process.stdout.write('PASS real revision comparison keeps dirty state and pinned commits\n');
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
}

main().catch(error => { console.error(error.stack || String(error)); process.exitCode = 1; });
