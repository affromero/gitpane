'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const { EventEmitter, getEventListeners } = require('node:events');
const { PassThrough } = require('node:stream');
const { GitContentWorkers, captureGitEnvironment } = require('../src/git-content-worker');

function workerScript(source, options = {}) {
  return new GitContentWorkers({ ...options, spawn: (_executable, _args, childOptions) =>
    spawn(process.execPath, ['-e', source], childOptions) });
}

const request = operation => ({ version: 1, operation });

test('content worker preserves binary stdout and rejects structured worker errors', async t => {
  const success = workerScript("process.stdin.once('data', () => { process.stdout.write(Buffer.from([0, 255, 10])); process.exitCode = 0; process.stdin.destroy(); });");
  t.after(() => success.dispose());
  assert.deepEqual(await success.run('unused', process.env, request({ kind: 'verified_content' })), Buffer.from([0, 255, 10]));
  const failure = workerScript("process.stdin.once('data', () => { process.stdout.write('partial content'); process.stderr.write(JSON.stringify({code:'stale_target',message:'Owner changed'})); process.exitCode = 1; process.stdin.destroy(); });");
  t.after(() => failure.dispose());
  await assert.rejects(failure.run('unused', process.env, request({ kind: 'verified_content' })),
    error => error.code === 'stale_target' && /owner changed/i.test(error.message));
});

test('aborting a content request closes its liveness pipe', async t => {
  const worker = workerScript("process.stdin.resume(); process.stdin.on('end', () => process.exit(7));");
  t.after(() => worker.dispose());
  const controller = new AbortController();
  const reading = worker.run('unused', process.env, request({ kind: 'verified_content' }), controller.signal);
  controller.abort();
  await assert.rejects(reading, error => error.code === 'cancelled');
});

test('cancellation during worker spawn is not missed', async t => {
  const controller = new AbortController();
  const worker = new GitContentWorkers({ spawn: (_executable, _args, options) => {
    const child = spawn(process.execPath, ['-e', "process.stdin.resume(); process.stdin.on('end', () => process.exit(7));"], options);
    controller.abort();
    return child;
  } });
  t.after(() => worker.dispose());
  await assert.rejects(worker.run('unused', process.env, request({ kind: 'verified_content' }), controller.signal),
    error => error.code === 'cancelled');
});

test('a signal-terminated worker rejects the request and removes its abort listener', { skip: process.platform === 'win32' }, async t => {
  const worker = workerScript("process.stdin.once('data', () => process.kill(process.pid, 'SIGTERM'));");
  t.after(() => worker.dispose());
  const controller = new AbortController();
  await assert.rejects(worker.run('unused', process.env, request({ kind: 'verified_content' }), controller.signal),
    error => error.code === 'protocol_error' && /signal SIGTERM/i.test(error.message));
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
});

test('cancellation does not signal a supervisor after it exits while its pipes remain open', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const child = new EventEmitter();
  child.pid = 12345;
  child.stdin = new PassThrough();
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  let signals = 0;
  child.kill = () => { signals++; };
  const worker = new GitContentWorkers({ spawn: () => child, killGroup: () => { signals++; } });
  t.after(() => worker.dispose());
  const controller = new AbortController();
  const reading = worker.run('unused', process.env, request({ kind: 'verified_content' }), controller.signal);
  const rejected = assert.rejects(reading, error => error.code === 'cancelled');
  controller.abort();
  child.emit('exit', 0, null);
  t.mock.timers.tick(1000);
  assert.equal(signals, 0);
  child.emit('close', 0, null);
  await rejected;
});

test('selected Git environment removes repository routing variables', () => {
  const git = { git: { path: process.execPath, env: { GIT_DIR: '/wrong', GIT_INDEX_FILE: '/wrong-index', GIT_CONFIG_GLOBAL: '/config' } } };
  const captured = captureGitEnvironment(git);
  assert.equal(captured.executable, process.execPath);
  assert.equal(captured.env.GIT_DIR, undefined);
  assert.equal(captured.env.GIT_INDEX_FILE, undefined);
  assert.equal(captured.env.GIT_CONFIG_GLOBAL, '/config');
  assert.equal(captured.key, captureGitEnvironment(git).key);
  git.git.env.GIT_CONFIG_GLOBAL = '/different';
  assert.notEqual(captured.key, captureGitEnvironment(git).key);
});
