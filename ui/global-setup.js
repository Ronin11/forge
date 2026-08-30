// Global setup: build forge, start a daemon with a temp FORGE_HOME, record the
// empty-state rendering of every page, then seed fixture data over the HTTP API
// (see seed.mjs). Returns the global teardown (kill the daemon, remove the home).
const { spawn, execSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const HOME = path.join(__dirname, '.tmp-home');
const BASE = 'http://127.0.0.1:7346'; // NOT 7340: a live dev daemon may own it

// Every page must render its empty state before any data exists. Worker cards are
// deliberately not asserted here: the daemon spawns its own local worker, which
// registers at an arbitrary moment after start.
const EMPTY_CHECKS = [
  { path: '/', wants: ['No tasks yet', 'Nothing running.'] },
  { path: '/tasks', wants: ['No tasks yet'] },
  { path: '/queue', wants: ['The queue is empty.'] },
  { path: '/attention', wants: ['Nothing is waiting on you.'] },
  { path: '/proposals', wants: ['No proposals yet'] },
  { path: '/routines', wants: ['No routines'] },
  { path: '/stats', wants: ['No finished attempts in this window.'] },
  { path: '/system', wants: ['No repositories advertised'] },
];

async function waitForHealthz(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const res = await fetch(BASE + '/healthz');
      if (res.ok) return;
    } catch {
      // not up yet
    }
    if (Date.now() > deadline) throw new Error(`daemon did not answer ${BASE}/healthz within ${timeoutMs}ms`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

async function emptyStatePass() {
  const results = [];
  for (const check of EMPTY_CHECKS) {
    const res = await fetch(BASE + check.path);
    const body = await res.text();
    for (const want of check.wants) {
      results.push({ path: check.path, status: res.status, want, found: body.includes(want) });
    }
  }
  fs.writeFileSync(path.join(HOME, 'empty-states.json'), JSON.stringify(results, null, 2));
}

function stopDaemon(daemon) {
  return new Promise((resolve) => {
    if (daemon.exitCode !== null || daemon.signalCode !== null) return resolve();
    const killTimer = setTimeout(() => daemon.kill('SIGKILL'), 5000);
    daemon.once('exit', () => {
      clearTimeout(killTimer);
      resolve();
    });
    daemon.kill('SIGTERM');
  });
}

module.exports = async function globalSetup() {
  fs.rmSync(HOME, { recursive: true, force: true });
  fs.mkdirSync(HOME, { recursive: true });
  execSync('go build -o .tmp-home/forge ../cmd/forge', { cwd: __dirname, stdio: 'inherit' });

  // INVOCATION_ID is dropped so a daemon started from inside a systemd unit
  // (some terminals set it) never tries `systemctl start forge-worker`.
  const env = {
    ...process.env,
    FORGE_HOME: HOME,
    FORGE_HTTP: '127.0.0.1:7346',
    FORGE_LOG_LEVEL: 'warn',
  };
  delete env.INVOCATION_ID;
  const daemon = spawn(path.join(HOME, 'forge'), ['daemon', 'start', '--foreground'], {
    env,
    stdio: ['ignore', 'inherit', 'inherit'],
  });
  fs.writeFileSync(path.join(HOME, 'daemon.pid.ui-test'), String(daemon.pid));

  try {
    await waitForHealthz(30_000);
    await emptyStatePass();
    const { seed } = await import('./seed.mjs');
    const seeded = await seed(BASE, HOME);
    fs.writeFileSync(path.join(HOME, 'seed.json'), JSON.stringify(seeded, null, 2));
  } catch (err) {
    await stopDaemon(daemon);
    throw err;
  }

  return async function globalTeardown() {
    await stopDaemon(daemon);
    fs.rmSync(HOME, { recursive: true, force: true });
  };
};
