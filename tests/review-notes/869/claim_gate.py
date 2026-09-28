import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile

binary = Path('target/debug/forge').resolve()
with tempfile.TemporaryDirectory(prefix='forge-review-869-') as scratch:
    root = Path(scratch)
    repo = root / 'repo'
    home = root / 'home'
    repo.mkdir()
    home.mkdir()
    def git(*args):
        subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True)
    git('init', '-q', '-b', 'main')
    git('config', 'user.name', 'Review')
    git('config', 'user.email', 'review@example.com')
    (repo / 'forge.toml').write_text('[checks]\nok = ["true"]\n[execution]\nbackend = "host"\n')
    git('add', '.')
    git('commit', '-qm', 'fixture')
    config = home / 'config.toml'
    config.write_text('[trust.public]\nallow_unsandboxed = true\n')
    env = {k: v for k, v in os.environ.items() if not k.startswith('FORGE_')}
    env.update(FORGE_HOME=str(home), FORGE_SANDBOX='0', FORGE_SUPERVISOR='0',
               FORGE_CLAUDE_BIN=str(root / 'missing-agent'), XDG_CONFIG_HOME=str(root / 'xdg'))
    def forge(*args):
        result = subprocess.run([str(binary), *args], env=env, text=True, capture_output=True, timeout=30)
        print('forge', *args, 'exit', result.returncode)
        print(result.stdout, result.stderr)
        return result
    assert forge('add', str(repo), 'review fixture', '--trust', 'public', '--workflow', 'reviewed', '--no-land').returncode == 0
    # Revoke the opt-out before claim: this task must now block on host.
    config.write_text('[trust.public]\nallow_unsandboxed = false\n')
    forge('work', '--once')
    with sqlite3.connect(home / 'forge.db') as db:
        state, reason = db.execute('SELECT state, reason FROM tasks WHERE id=1').fetchone()
        attempts = db.execute('SELECT COUNT(*) FROM attempts WHERE task_id=1').fetchone()[0]
    print('ACTUAL:', state, reason, 'attempts:', attempts)
    assert state == 'blocked' and 'host' in reason and 'public' in reason, (state, reason)
