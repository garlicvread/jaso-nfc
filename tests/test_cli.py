import contextlib
import io
import json
import plistlib
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from jaso_nfc.config import Config
from jaso_nfc.service import launch_agent, install_files
from jaso_nfc.cli import main, configuration, parser
from jaso_nfc import service


class InstallerLifecycleTests(unittest.TestCase):
    @contextlib.contextmanager
    def holding_worker(self, config, delay=0.1):
        service.prepare_directories(config)
        code = """import fcntl, sys, time
with open(sys.argv[1], 'a') as handle:
    fcntl.flock(handle, fcntl.LOCK_EX)
    print('locked', flush=True)
    sys.stdin.readline()
    time.sleep(float(sys.argv[2]))
"""
        worker = subprocess.Popen([sys.executable, '-c', code,
                                   str(Path(config.state_dir) / '.lock'), str(delay)],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, text=True)
        try:
            self.assertEqual(worker.stdout.readline(), 'locked\n')
            yield worker
        finally:
            if worker.poll() is None:
                worker.terminate()
            worker.communicate(timeout=5)

    def test_install_waits_for_stopped_process_to_release_real_runtime_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(roots=[tmp], state_dir=tmp + '/support')
            original_stage = service.install_files
            staged = []

            def stage(value):
                self.assertTrue(service.runtime_status(config)['running'])
                staged.append(True)
                return original_stage(value)

            def bootstrap(command, **kwargs):
                self.assertEqual(command[1], 'bootstrap')
                self.assertEqual(staged, [True])
                # The new worker must be able to acquire its lock immediately.
                with service.runtime_lock(config):
                    pass
                return subprocess.CompletedProcess(command, 0, '', '')

            with self.holding_worker(config) as worker:
                def stop():
                    worker.stdin.write('stop\n')
                    worker.stdin.flush()
                    return subprocess.CompletedProcess([], 0, '', '')

                with patch.object(Path, 'home', return_value=Path(tmp)), \
                        patch.object(service.sys, 'platform', 'darwin'), \
                        patch.object(service, 'stop_agent', side_effect=stop), \
                        patch.object(service, 'install_files', side_effect=stage), \
                        patch.object(service.subprocess, 'run', side_effect=bootstrap):
                    try:
                        installed = service.install(config)
                    except RuntimeError as error:
                        self.fail('installation did not wait for shutdown: ' + str(error))
            self.assertTrue(installed.exists())
            self.assertEqual(Config.load(Path(config.state_dir) / 'config.json'), config)

    def test_install_timeout_does_not_stage_rewrite_or_start_a_service(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(roots=[tmp], state_dir=tmp + '/support')
            plist = Path(tmp) / 'Library/LaunchAgents' / f'{service.LABEL}.plist'
            plist.parent.mkdir(parents=True)
            plist.write_bytes(b'original plist')
            config_path = Path(config.state_dir) / 'config.json'
            config.save(config_path)
            old_time = config_path.stat().st_mtime_ns
            with self.holding_worker(config), \
                    patch.object(Path, 'home', return_value=Path(tmp)), \
                    patch.object(service.sys, 'platform', 'darwin'), \
                    patch.object(service, 'INSTALL_STOP_TIMEOUT', 0.05, create=True), \
                    patch.object(service, 'stop_agent', return_value=subprocess.CompletedProcess([], 0)), \
                    patch.object(service, 'install_files') as stage, \
                    patch.object(service.subprocess, 'run') as bootstrap:
                with self.assertRaises(RuntimeError) as failure:
                    service.install(config)
                stage.assert_not_called()
                bootstrap.assert_not_called()
                self.assertIn('Timed out', str(failure.exception))
            self.assertEqual(config_path.stat().st_mtime_ns, old_time)
            self.assertEqual(plist.read_bytes(), b'original plist')

    def test_default_runtime_lock_remains_immediate(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(roots=[tmp], state_dir=tmp + '/support')
            with self.holding_worker(config), \
                    patch.object(service.time, 'sleep', side_effect=AssertionError('unexpected lock wait')):
                with self.assertRaisesRegex(RuntimeError, 'already running'):
                    with service.runtime_lock(config):
                        self.fail('entered lock while old worker still held it')

    def test_install_stop_failure_cannot_stage_or_bootstrap(self):
        with tempfile.TemporaryDirectory() as tmp, \
                patch.object(Path, 'home', return_value=Path(tmp)), \
                patch.object(service.sys, 'platform', 'darwin'), \
                patch.object(service, 'stop_agent', return_value=subprocess.CompletedProcess(
                    [], 5, '', 'fixture stop failed')), \
                patch.object(service, 'install_files') as stage, \
                patch.object(service.subprocess, 'run') as bootstrap:
            with self.assertRaisesRegex(RuntimeError, 'fixture stop failed'):
                service.install(Config(roots=[tmp], state_dir=tmp + '/support'))
            stage.assert_not_called()
            bootstrap.assert_not_called()

    def test_failed_bootstrap_restores_configuration_under_runtime_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            old = Config(roots=[tmp], state_dir=tmp + '/support')
            config_path = Path(old.state_dir) / 'config.json'
            old.save(config_path)
            original_config = config_path.read_bytes()
            plist = Path(tmp) / 'Library/LaunchAgents' / f'{service.LABEL}.plist'
            plist.parent.mkdir(parents=True)
            plist.write_bytes(b'original plist')
            changed = Config(roots=[tmp], state_dir=old.state_dir, apply=True)
            original_write = Path.write_bytes
            locked_restorations = []

            def observe_write(path, data):
                if path in (config_path, plist):
                    locked_restorations.append(service.runtime_status(changed)['running'])
                return original_write(path, data)

            with patch.object(Path, 'home', return_value=Path(tmp)), \
                    patch.object(Path, 'write_bytes', observe_write), \
                    patch.object(service.sys, 'platform', 'darwin'), \
                    patch.object(service, 'stop_agent', return_value=subprocess.CompletedProcess([], 0)), \
                    patch.object(service.subprocess, 'run', side_effect=[
                        subprocess.CompletedProcess([], 5, '', 'fixture bootstrap failed'),
                        subprocess.CompletedProcess([], 0, '', '')]):
                with self.assertRaisesRegex(RuntimeError, 'fixture bootstrap failed'):
                    service.install(changed)
            self.assertEqual(locked_restorations, [True, True])
            self.assertEqual(config_path.read_bytes(), original_config)
            self.assertEqual(plist.read_bytes(), b'original plist')


class ConfigurationTests(unittest.TestCase):
    def test_all_user_files_is_explicit_and_does_not_keep_fixed_roots(self):
        args = parser().parse_args(['install', '--all-user-files', '--apply'])
        config = configuration(args)
        self.assertEqual(config.scope, 'all-user-files')
        self.assertEqual(config.roots, [])
        self.assertEqual(config.excludes, [])
        self.assertTrue(config.apply)

    def test_scope_selector_rejects_simultaneous_fixed_roots(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            parser().parse_args(['watch', '--all-user-files', '--root', '/example'])

    def test_state_dir_lookup_preserves_saved_custom_log_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(state_dir=tmp + '/support', log_dir=tmp + '/custom-logs')
            config.save(Path(config.state_dir) / 'config.json')
            args = parser().parse_args(['status', '--state-dir', config.state_dir])
            self.assertEqual(configuration(args).log_dir, config.log_dir)

    def test_invalid_watch_config_has_bootstrap_diagnostic(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'config.json'
            path.write_text('invalid json')
            with contextlib.redirect_stderr(io.StringIO()):
                result = main(['watch', '--config', str(path)])
            self.assertEqual(result, 1)
            self.assertIn('JSONDecodeError', (Path(tmp) / 'logs/service.log').read_text())

    def test_status_distinguishes_stopped_index_from_running_worker(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(state_dir=tmp)
            self.assertFalse(service.runtime_status(config)['running'])
            with service.runtime_lock(config):
                self.assertTrue(service.runtime_status(config)['running'])
            self.assertFalse(service.runtime_status(config)['running'])

    def test_status_shows_future_rename_retries(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(state_dir=tmp)
            service.prepare_directories(config)
            config.state_path('skip.json').write_text(json.dumps({
                'version': 1, 'entries': {'example': {'next_retry': 12345}}}))
            state = service.runtime_status(config)
            self.assertEqual(state['deferred_renames'], 1)
            self.assertEqual(state['next_rename_retry'], 12345)

    def test_custom_support_directory_owns_its_default_logs_and_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(state_dir=tmp)
            self.assertEqual(config.log_dir, str(Path(tmp) / 'logs'))
            self.assertEqual(config.state_path('index.sqlite3'), Path(tmp) / 'state/index.sqlite3')

    def test_uninstall_does_not_claim_stopped_on_launchctl_failure(self):
        failure = subprocess.CompletedProcess([], 5, '', 'I/O error')
        with tempfile.TemporaryDirectory() as tmp, patch.object(Path, 'home', return_value=Path(tmp)):
            plist = Path(tmp) / 'Library/LaunchAgents' / f'{service.LABEL}.plist'
            plist.parent.mkdir(parents=True)
            plist.write_bytes(b'existing')
            with patch.object(service, 'stop_agent', return_value=failure):
                with self.assertRaises(RuntimeError):
                    service.uninstall()
            self.assertTrue(plist.exists())

    def test_watch_database_failure_is_persisted_to_bounded_log(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(roots=[tmp], state_dir=tmp + '/state', log_dir=tmp + '/logs')
            path = Path(tmp) / 'config.json'
            config.save(path)
            with patch.object(service, 'watch', side_effect=sqlite3.OperationalError('locked fixture')):
                with contextlib.redirect_stderr(io.StringIO()):
                    result = main(['watch', '--config', str(path)])
            self.assertEqual(result, 1)
            self.assertIn('locked fixture', (Path(config.log_dir) / 'service.log').read_text())

    def test_config_roundtrip_preserves_scope(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(roots=[tmp], excludes=[tmp + '/excluded'],
                            state_dir=tmp + '/state', log_dir=tmp + '/logs')
            path = Path(tmp) / 'config.json'
            config.save(path)
            self.assertEqual(Config.load(path), config)
            self.assertFalse(config.apply)

    def test_configuration_rejects_unknown_fields(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'config.json'
            path.write_text('{"unexpected":true}')
            with self.assertRaises(ValueError):
                Config.load(path)

    def test_plist_is_continuous_and_keeps_all_roots_in_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(roots=[tmp + '/a', tmp + '/b'], state_dir=tmp,
                            log_dir=tmp + '/logs', apply=True)
            value = plistlib.loads(launch_agent(config, '/python', '/runner.py'))
            self.assertNotIn('StartInterval', value)
            self.assertNotIn('WatchPaths', value)
            self.assertTrue(value['KeepAlive'])
            self.assertIn('watch', value['ProgramArguments'])
            self.assertEqual(config.roots, [tmp + '/a', tmp + '/b'])

    def test_install_stages_package_and_retains_history(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = Config(roots=[tmp], state_dir=tmp + '/state', log_dir=tmp + '/logs')
            Path(config.log_dir).mkdir()
            history = Path(config.log_dir) / 'renames.jsonl'
            history.write_text('historical records')
            plist = install_files(config, Path(tmp) / 'LaunchAgents')
            self.assertTrue(plist.exists())
            value = plistlib.loads(plist.read_bytes())
            self.assertTrue(Path(value['ProgramArguments'][1]).is_file())
            self.assertEqual(history.read_text(), 'historical records')
            self.assertEqual(Config.load(Path(config.state_dir) / 'config.json'), config)


if __name__ == '__main__':
    unittest.main()
