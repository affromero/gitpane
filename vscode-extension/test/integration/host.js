'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');

// Run a dedicated VS Code process and terminate only that process after its
// extension-host suite atomically publishes the result file.
function runHost(code, args, env, timeoutMs) {
  return new Promise((resolve, reject) => {
    const child = spawn(code, args, { env, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    let output = '';
    let completed = false;
    let timedOut = false;
    let forceTimer;
    child.stdout.on('data', chunk => { output += chunk.toString(); });
    child.stderr.on('data', chunk => { output += chunk.toString(); });
    const deadline = setTimeout(() => {
      timedOut = true;
      stop();
    }, timeoutMs);
    const poll = setInterval(() => {
      if (!fs.existsSync(env.GITPANE_TEST_RESULT)) return;
      completed = true;
      stop();
    }, 100);
    function stop() {
      clearInterval(poll);
      child.kill('SIGTERM');
      forceTimer = setTimeout(() => child.kill('SIGKILL'), 5000);
    }
    function clear() {
      clearTimeout(deadline);
      clearInterval(poll);
      clearTimeout(forceTimer);
    }
    child.on('error', error => { clear(); reject(error); });
    child.on('close', (status, signal) => {
      clear();
      if (timedOut) return reject(new Error(`VS Code host timed out after ${timeoutMs} ms.\n${output}`));
      if (!completed && !fs.existsSync(env.GITPANE_TEST_RESULT)) {
        return reject(new Error(`VS Code exited before writing test results (status ${status}, signal ${signal}).\n${output}`));
      }
      if (!completed && status !== 0) return reject(new Error(`VS Code host exited with status ${status}.\n${output}`));
      resolve({ output, status, signal });
    });
  });
}

function publishResult(destination, data) {
  const temporary = `${destination}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, JSON.stringify(data, null, 2));
  fs.renameSync(temporary, destination);
}

function codeExecutable() {
  const configured = process.env.VSCODE_EXECUTABLE;
  if (!configured && process.platform === 'darwin') return '/Applications/Visual Studio Code.app/Contents/MacOS/Code';
  if (!configured) return 'code';
  if (process.platform === 'darwin') {
    const real = fs.realpathSync(configured);
    const suffix = `${path.sep}Contents${path.sep}Resources${path.sep}app${path.sep}bin${path.sep}code`;
    if (real.endsWith(suffix)) return path.join(real.slice(0, -suffix.length), 'Contents', 'MacOS', 'Code');
  }
  return configured;
}

module.exports = { codeExecutable, publishResult, runHost };
