'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const { spawn, spawnSync } = require('node:child_process');
const { startContainer } = require('./integration/container-host');

const docker = process.env.GITPANE_TEST_DOCKER || 'docker';
const imageName = 'node:22-bookworm-slim';

function command(args) {
  return spawnSync(docker, ['--host', `unix://${fs.realpathSync('/var/run/docker.sock')}`, ...args], { encoding: 'utf8' });
}

function available(t) {
  try {
    const result = command(['image', 'inspect', '--format', '{{.Id}}', imageName]);
    const image = result.stdout?.trim();
    if (!result.error && result.status === 0 && /^sha256:[a-f0-9]{64}$/.test(image)) return image;
  } catch {}
  t.skip('A local Docker socket and pre-staged node:22-bookworm-slim image are required.');
  return undefined;
}

function fixture(t, image) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-container-fixture-'));
  const source = path.join(root, 'source');
  const integration = path.join(source, 'vscode-extension', 'test', 'integration');
  const extension = path.join(root, 'extension');
  fs.mkdirSync(integration, { recursive: true });
  fs.mkdirSync(extension);
  fs.writeFileSync(path.join(integration, 'fixture.js'), 'fixture\n');
  fs.writeFileSync(path.join(extension, 'package.json'), '{}\n');
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return { image, source, extension, timeoutSeconds: 10 };
}

async function waitFor(condition, message, timeout = 15000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    if (condition()) return;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error(message);
}

function runningFor(label) {
  const result = command(['ps', '-aq', '--no-trunc', '--filter', `label=gitpane.host-test=${label}`]);
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

function cleanupOwned(root) {
  if (!fs.existsSync(path.join(root, 'container.id'))) return;
  const id = fs.readFileSync(path.join(root, 'container.id'), 'utf8').trim();
  assert.match(id, /^[a-f0-9]{64}$/);
  const inspected = command(['inspect', '--format', '{{json .}}', id]);
  assert.equal(inspected.status, 0, inspected.stderr);
  const container = JSON.parse(inspected.stdout);
  const config = JSON.parse(fs.readFileSync(path.join(root, 'config.json'), 'utf8'));
  assert.equal(container.Id, id);
  assert.equal(container.Config.Labels['gitpane.host-test'], config.token);
  assert.equal(container.Image, config.image);
  const mounts = new Map(container.Mounts.map(item => [item.Destination, item]));
  for (const [destination, source, writable] of [['/source', config.source, false],
    ['/extension', config.extension, false], ['/output', config.results, true]]) {
    assert.equal(mounts.get(destination)?.Source, source);
    assert.equal(mounts.get(destination)?.RW, writable);
  }
  if (container.State.Running) {
    const stopped = command(['stop', '--time', '1', id]);
    assert.equal(stopped.status, 0, stopped.stderr);
  }
  const removed = command(['rm', id]);
  assert.equal(removed.status, 0, removed.stderr);
  fs.rmSync(root, { recursive: true, force: true });
}

function cleanRun(t, run) {
  t.after(async () => {
    if (run.child.exitCode === null && run.child.signalCode === null) run.cancel();
    let timer;
    try {
      const result = await Promise.race([run.completion, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Cleanup timed out')), 15000); })]);
      if (result.cleanupVerified) fs.rmSync(run.root, { recursive: true, force: true });
      else console.error(`Preserved unverified container artifacts: ${run.root}`);
    } catch { console.error(`Preserved unverified container artifacts: ${run.root}`); }
    finally { clearTimeout(timer); }
  });
}

test('container runner isolates and cleans up a normal dummy host', { timeout: 30000 }, async t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const uid = process.getuid() === 0 ? 65534 : process.getuid();
  const script = `const fs=require('node:fs');
    if(process.getuid()!==${uid})throw Error('wrong UID');
    for(const key of ['XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','XDG_RUNTIME_DIR'])fs.writeFileSync(process.env[key]+'/probe','ok');
    try{fs.writeFileSync('/etc/gitpane-probe','bad');throw Error('root filesystem was writable');}catch(error){if(error.message==='root filesystem was writable')throw error;}
    console.log('dummy host stdout');console.error('dummy host stderr');process.stderr.write('x'.repeat(1024*1024+30));`;
  const run = startContainer({ ...options, command: ['node', '-e', script] });
  cleanRun(t, run);
  await run.ready;
  run.start();
  const result = await run.completion;
  assert.equal(result.success, true, result.error);
  assert.equal(result.cleanupVerified, true);
  assert.equal(runningFor(result.label), '');
  const logs = fs.readFileSync(path.join(run.root, 'container.log'), 'utf8');
  assert.match(logs, /dummy host stdout/);
  assert.match(logs, /dummy host stderr/);
  assert.match(logs, /\[stderr truncated\]/);
  for (const name of ['config', 'cache', 'data', 'runtime']) {
    assert.equal(fs.readFileSync(path.join(run.root, 'results', name, 'probe'), 'utf8'), 'ok');
  }
});

test('staging refuses a temporary directory inside its extension input', t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const previous = process.env.TMPDIR;
  process.env.TMPDIR = options.extension;
  try {
    assert.throws(() => startContainer({ ...options, command: ['node', '-e', ''] }), /temporary directory cannot be inside/i);
  } finally {
    if (previous === undefined) delete process.env.TMPDIR;
    else process.env.TMPDIR = previous;
  }
});

test('staging rejects symlinks and inputs larger than its byte limit', t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const link = path.join(options.extension, 'link');
  fs.symlinkSync(path.join(options.extension, 'package.json'), link);
  assert.throws(() => startContainer({ ...options, command: ['node', '-e', ''] }), /symlinks/i);
  fs.unlinkSync(link);
  const large = path.join(options.extension, 'large');
  fs.closeSync(fs.openSync(large, 'w'));
  fs.truncateSync(large, 256 * 1024 * 1024 + 1);
  assert.throws(() => startContainer({ ...options, command: ['node', '-e', ''] }), /staging size limit/i);
});

test('disconnect stops a detached child that holds the host output pipes', { timeout: 30000 }, async t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const script = `const fs=require('node:fs'),{spawn}=require('node:child_process');
    const child=spawn(process.execPath,['-e',"setTimeout(()=>require('node:fs').writeFileSync('/output/late','leaked'),5000)"],{detached:true,stdio:'inherit'});
    child.unref();fs.writeFileSync('/output/ready','ready');process.on('SIGTERM',()=>{});setInterval(()=>{},1000);`;
  const run = startContainer({ ...options, command: ['node', '-e', script] });
  cleanRun(t, run);
  await run.ready;
  run.start();
  await waitFor(() => fs.existsSync(path.join(run.root, 'results', 'ready')), 'Dummy host did not start.');
  run.cancel();
  const result = await run.completion;
  assert.equal(result.success, false);
  assert.equal(result.cleanupVerified, true, result.error);
  assert.equal(runningFor(result.label), '');
  await new Promise(resolve => setTimeout(resolve, 5200));
  assert.equal(fs.existsSync(path.join(run.root, 'results', 'late')), false, 'detached child survived container cleanup');
});

test('the internal watchdog stops a detached child without touching a sibling container', { timeout: 30000 }, async t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const sibling = command(['create', '--pull=never', '--init', '--network=none',
    '--label', `gitpane.host-sibling=${crypto.randomBytes(16).toString('hex')}`,
    '--entrypoint', 'node', image, '-e', 'setInterval(()=>{},1000)']);
  assert.equal(sibling.status, 0, sibling.stderr);
  const siblingId = sibling.stdout.trim();
  assert.match(siblingId, /^[a-f0-9]{64}$/);
  t.after(() => {
    const stopped = command(['stop', '--time', '1', siblingId]);
    assert.equal(stopped.status, 0, stopped.stderr);
    const removed = command(['rm', siblingId]);
    assert.equal(removed.status, 0, removed.stderr);
  });
  const started = command(['start', siblingId]);
  assert.equal(started.status, 0, started.stderr);
  const script = `const fs=require('node:fs'),{spawn}=require('node:child_process');
    const child=spawn(process.execPath,['-e',"setTimeout(()=>require('node:fs').writeFileSync('/output/late','leaked'),5000)"],{detached:true,stdio:'inherit'});
    child.unref();fs.writeFileSync('/output/ready','ready');process.on('SIGTERM',()=>{});setInterval(()=>{},1000);`;
  const run = startContainer({ ...options, timeoutSeconds: 1, graceSeconds: 1, command: ['node', '-e', script] });
  cleanRun(t, run);
  await run.ready;
  run.start();
  const result = await run.completion;
  assert.equal(result.success, false);
  assert.equal(result.cleanupVerified, true, result.error);
  assert.equal(runningFor(result.label), '');
  const state = command(['inspect', '--format', '{{.State.Running}}', siblingId]);
  assert.equal(state.status, 0, state.stderr);
  assert.equal(state.stdout.trim(), 'true', 'an unrelated container was stopped');
  await new Promise(resolve => setTimeout(resolve, 5200));
  assert.equal(fs.existsSync(path.join(run.root, 'results', 'late')), false, 'watchdog left a detached child running');
});

test('a killed caller leaves the supervisor to stop its own container', { timeout: 30000 }, async t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const helper = path.join(options.source, 'helper.js');
  const modulePath = path.resolve(__dirname, 'integration/container-host.js');
  fs.writeFileSync(helper, `const {startContainer}=require(${JSON.stringify(modulePath)});\n` +
    `const options=${JSON.stringify(options)};\n` +
    `const script="const fs=require('node:fs'),{spawn}=require('node:child_process');const c=spawn(process.execPath,['-e',\\\"setTimeout(()=>require('node:fs').writeFileSync('/output/late','leaked'),5000)\\\"],{detached:true,stdio:'inherit'});c.unref();fs.writeFileSync('/output/ready','ready');process.on('SIGTERM',()=>{});setInterval(()=>{},1000)";\n` +
    `const run=startContainer({...options,command:['node','-e',script]});console.log(run.root);run.ready.then(()=>run.start());setInterval(()=>{},1000);\n`);
  const child = spawn(process.execPath, [helper], { stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  child.stdout.on('data', data => { output += data.toString(); });
  await waitFor(() => output.includes('\n'), 'Caller did not report its artifact directory.');
  const root = output.trim().split('\n')[0];
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
    try {
      await waitFor(() => fs.existsSync(path.join(root, 'result.json')), 'Cleanup timed out.', 15000);
      const result = JSON.parse(fs.readFileSync(path.join(root, 'result.json'), 'utf8'));
      if (result.cleanupVerified) fs.rmSync(root, { recursive: true, force: true });
      else console.error(`Preserved unverified container artifacts: ${root}`);
    } catch { console.error(`Preserved unverified container artifacts: ${root}`); }
  });
  await waitFor(() => fs.existsSync(path.join(root, 'results', 'ready')), 'Dummy host did not start.');
  child.kill('SIGKILL');
  await waitFor(() => fs.existsSync(path.join(root, 'result.json')), 'Supervisor did not finish cleanup.');
  const result = JSON.parse(fs.readFileSync(path.join(root, 'result.json'), 'utf8'));
  assert.equal(result.success, false);
  assert.equal(result.cleanupVerified, true, result.error);
  assert.equal(runningFor(result.label), '');
  await new Promise(resolve => setTimeout(resolve, 5200));
  assert.equal(fs.existsSync(path.join(root, 'results', 'late')), false, 'detached child survived caller death');
});

test('a caller killed during create leaves the supervisor to clean the eventual container', { timeout: 30000 }, async t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const real = spawnSync('which', ['docker'], { encoding: 'utf8' });
  assert.equal(real.status, 0, real.stderr);
  const realDocker = fs.realpathSync(real.stdout.trim());
  const marker = path.join(options.source, 'creating');
  const wrapper = path.join(options.source, 'docker-wrapper.js');
  fs.writeFileSync(wrapper, `#!/usr/bin/env node\n` +
    `const fs=require('node:fs'),{spawn}=require('node:child_process');\n` +
    `const run=()=>{const p=spawn(${JSON.stringify(realDocker)},process.argv.slice(2),{stdio:'inherit'});p.on('exit',code=>process.exit(code??1));};\n` +
    `if(process.argv[4]==='create'){fs.writeFileSync(${JSON.stringify(marker)},'creating');setTimeout(run,1500);}else run();\n`);
  fs.chmodSync(wrapper, 0o755);
  const helper = path.join(options.source, 'caller.js');
  const modulePath = path.resolve(__dirname, 'integration/container-host.js');
  fs.writeFileSync(helper, `const {startContainer}=require(${JSON.stringify(modulePath)});\n` +
    `const options=${JSON.stringify({ ...options, dockerPath: wrapper })};\n` +
    `const run=startContainer({...options,command:['node','-e','setInterval(()=>{},1000)']});console.log(run.root);run.ready.then(()=>run.start());setInterval(()=>{},1000);\n`);
  const child = spawn(process.execPath, [helper], { stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  child.stdout.on('data', data => { output += data.toString(); });
  await waitFor(() => output.includes('\n'), 'Caller did not report its artifact directory.');
  const root = output.trim().split('\n')[0];
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
    try {
      await waitFor(() => fs.existsSync(path.join(root, 'result.json')), 'Cleanup timed out.', 15000);
      const result = JSON.parse(fs.readFileSync(path.join(root, 'result.json'), 'utf8'));
      if (result.cleanupVerified) fs.rmSync(root, { recursive: true, force: true });
      else console.error(`Preserved unverified container artifacts: ${root}`);
    } catch { console.error(`Preserved unverified container artifacts: ${root}`); }
  });
  await waitFor(() => fs.existsSync(marker), 'Docker create did not start.');
  child.kill('SIGKILL');
  await waitFor(() => fs.existsSync(path.join(root, 'result.json')), 'Supervisor did not finish create cleanup.');
  const result = JSON.parse(fs.readFileSync(path.join(root, 'result.json'), 'utf8'));
  assert.equal(result.success, false);
  assert.equal(result.cleanupVerified, true, result.error);
  assert.equal(runningFor(result.label), '');
});

test('unavailable Docker cleanup is reported and preserves test artifacts', { timeout: 30000 }, async t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const real = spawnSync('which', ['docker'], { encoding: 'utf8' });
  assert.equal(real.status, 0, real.stderr);
  const realDocker = fs.realpathSync(real.stdout.trim());
  const wrapper = path.join(options.source, 'docker-fail-ps.js');
  fs.writeFileSync(wrapper, `#!/usr/bin/env node\n` +
    `const {spawn}=require('node:child_process');\n` +
    `if(process.argv[4]==='ps'){console.error('injected daemon failure');process.exit(1);}\n` +
    `const p=spawn(${JSON.stringify(realDocker)},process.argv.slice(2),{stdio:'inherit'});p.on('exit',code=>process.exit(code??1));\n`);
  fs.chmodSync(wrapper, 0o755);
  const run = startContainer({ ...options, dockerPath: wrapper,
    command: ['node', '-e', "require('node:fs').writeFileSync('/output/diagnostic','retained')"] });
  t.after(() => cleanupOwned(run.root));
  await run.ready;
  run.start();
  const result = await run.completion;
  assert.equal(result.success, false);
  assert.equal(result.cleanupVerified, false);
  assert.match(result.error, /Cleanup could not be verified.*injected daemon failure/s);
  assert.equal(fs.readFileSync(path.join(run.root, 'results', 'diagnostic'), 'utf8'), 'retained');
});

test('a missing label does not hide an existing recorded container', { timeout: 30000 }, async t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const real = spawnSync('which', ['docker'], { encoding: 'utf8' });
  assert.equal(real.status, 0, real.stderr);
  const realDocker = fs.realpathSync(real.stdout.trim());
  const wrapper = path.join(options.source, 'docker-hide-label.js');
  fs.writeFileSync(wrapper, `#!/usr/bin/env node\n` +
    `const {spawn}=require('node:child_process');\n` +
    `if(process.argv[4]==='ps'&&process.argv.some(x=>x.startsWith('label=gitpane.host-test=')))process.exit(0);\n` +
    `const p=spawn(${JSON.stringify(realDocker)},process.argv.slice(2),{stdio:'inherit'});p.on('exit',code=>process.exit(code??1));\n`);
  fs.chmodSync(wrapper, 0o755);
  const run = startContainer({ ...options, dockerPath: wrapper, command: ['node', '-e', ''] });
  t.after(() => cleanupOwned(run.root));
  await run.ready;
  run.start();
  const result = await run.completion;
  assert.equal(result.success, false);
  assert.equal(result.cleanupVerified, false);
  assert.match(result.error, /recorded container exists without its expected test label/i);
  const id = fs.readFileSync(path.join(run.root, 'container.id'), 'utf8').trim();
  const state = command(['inspect', '--format', '{{.State.Running}}', id]);
  assert.equal(state.status, 0, state.stderr);
  assert.equal(state.stdout.trim(), 'false');
});

test('Docker timeout errors indicate when stderr was truncated', { timeout: 30000 }, async t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const real = spawnSync('which', ['docker'], { encoding: 'utf8' });
  assert.equal(real.status, 0, real.stderr);
  const realDocker = fs.realpathSync(real.stdout.trim());
  const wrapper = path.join(options.source, 'docker-slow-image.js');
  fs.writeFileSync(wrapper, `#!/usr/bin/env node\n` +
    `const {spawn}=require('node:child_process');\n` +
    `if(process.argv[4]==='image'){process.stderr.write('x'.repeat(1024*1024+100));setInterval(()=>{},1000);}else{\n` +
    `const p=spawn(${JSON.stringify(realDocker)},process.argv.slice(2),{stdio:'inherit'});p.on('exit',code=>process.exit(code??1));}\n`);
  fs.chmodSync(wrapper, 0o755);
  const run = startContainer({ ...options, dockerPath: wrapper, command: ['node', '-e', ''] });
  cleanRun(t, run);
  await run.ready;
  run.start();
  const result = await run.completion;
  assert.equal(result.success, false);
  assert.equal(result.cleanupVerified, true);
  assert.match(result.error, /docker image timed out/);
  assert.match(result.error, /\[stderr truncated\]/);
  assert.equal(runningFor(result.label), '');
});

test('failure to write the diagnostic log cannot leave the dummy host running', { timeout: 30000 }, async t => {
  const image = available(t);
  if (!image) return;
  const options = fixture(t, image);
  const script = "require('node:fs').writeFileSync('/output/ready','ready');process.on('SIGTERM',()=>{});setInterval(()=>{},1000)";
  const run = startContainer({ ...options, command: ['node', '-e', script] });
  let cleaned = false;
  t.after(() => {
    if (cleaned) fs.rmSync(run.root, { recursive: true, force: true });
    else cleanupOwned(run.root);
  });
  await run.ready;
  run.start();
  await waitFor(() => fs.existsSync(path.join(run.root, 'results', 'ready')), 'Dummy host did not start.');
  fs.mkdirSync(path.join(run.root, 'container.log'));
  run.cancel();
  const result = await run.completion;
  assert.equal(result.success, false);
  assert.equal(result.cleanupVerified, true, result.error);
  assert.match(result.error, /Could not save container logs.*EISDIR/s);
  const id = fs.readFileSync(path.join(run.root, 'container.id'), 'utf8').trim();
  const state = command(['ps', '-aq', '--no-trunc', '--filter', `id=${id}`]);
  assert.equal(state.status, 0, state.stderr);
  assert.equal(state.stdout.trim(), '', 'logging failure left the test container behind');
  cleaned = true;
});
