'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { isolatedGitEnv } = require('./integration/git-env');

test('integration Git commands cannot inherit another repository or its object store', t => {
  const version = spawnSync('git', ['--version'], { encoding: 'utf8' });
  if (version.error?.code === 'ENOENT') return t.skip('Git is not installed');
  assert.ifError(version.error);
  assert.equal(version.status, 0, version.stderr);

  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-test-env-'));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const control = path.join(temp, 'control');
  const fixture = path.join(temp, 'fixture');
  fs.mkdirSync(control);
  fs.mkdirSync(fixture);
  const clean = isolatedGitEnv();
  const git = (repo, env, ...args) => {
    const result = spawnSync('git', ['-C', repo, '-c', 'user.name=Gitpane Test',
      '-c', 'user.email=gitpane-test@example.invalid', ...args], { env, encoding: 'utf8' });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  git(control, clean, 'init', '-q');
  fs.writeFileSync(path.join(control, 'control.txt'), 'untouched\n');
  git(control, clean, 'add', '--', 'control.txt');
  git(control, clean, 'commit', '-qm', 'control');
  const controlHead = git(control, clean, 'rev-parse', 'HEAD');
  const controlObjects = git(control, clean, 'count-objects');
  const gitDir = path.join(control, '.git');
  const injected = {
    ...process.env, GIT_DIR: gitDir, GIT_WORK_TREE: control,
    GIT_INDEX_FILE: path.join(gitDir, 'index'), GIT_COMMON_DIR: gitDir,
    GIT_OBJECT_DIRECTORY: path.join(gitDir, 'objects'),
    GIT_ALTERNATE_OBJECT_DIRECTORIES: path.join(gitDir, 'objects'),
    GIT_PREFIX: 'elsewhere/', GIT_NAMESPACE: 'other', GIT_CONFIG_COUNT: '1',
    GIT_CONFIG_KEY_0: 'core.worktree', GIT_CONFIG_VALUE_0: control,
    GIT_CONFIG_PARAMETERS: 'invalid', GIT_TEMPLATE_DIR: path.join(temp, 'missing-template')
  };
  const env = isolatedGitEnv(injected);
  for (const key of Object.keys(injected).filter(key => key.startsWith('GIT_'))) {
    if (['GIT_CONFIG_NOSYSTEM', 'GIT_CONFIG_GLOBAL'].includes(key)) continue;
    if (['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'GIT_OBJECT_DIRECTORY',
      'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_PREFIX', 'GIT_NAMESPACE', 'GIT_CONFIG_COUNT',
      'GIT_CONFIG_KEY_0', 'GIT_CONFIG_VALUE_0', 'GIT_CONFIG_PARAMETERS', 'GIT_TEMPLATE_DIR'].includes(key)) {
      assert.equal(env[key], undefined, `${key} must be cleared`);
    }
  }
  git(fixture, env, 'init', '-q');
  fs.writeFileSync(path.join(fixture, 'fixture.txt'), 'fixture contents\n');
  git(fixture, env, 'add', '--', 'fixture.txt');
  git(fixture, env, 'commit', '-qm', 'fixture');
  assert.equal(git(fixture, clean, 'show', 'HEAD:fixture.txt'), 'fixture contents');
  assert.equal(git(control, clean, 'rev-parse', 'HEAD'), controlHead);
  assert.equal(git(control, clean, 'count-objects'), controlObjects);
  assert.equal(fs.readFileSync(path.join(control, 'control.txt'), 'utf8'), 'untouched\n');
});
