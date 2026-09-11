"""Command-line entry point."""

import argparse
from dataclasses import asdict
import json
import logging
from logging.handlers import RotatingFileHandler
from pathlib import Path
import sys

from . import __version__
from .config import Config


def parser():
    result = argparse.ArgumentParser(description='Index filenames once, then normalize changed names to NFC.')
    result.add_argument('--version', action='version', version=__version__)
    commands = result.add_subparsers(dest='command', required=True)
    for command, help_text in (
            ('scan', 'Inspect configured roots; add --apply to rename once'),
            ('watch', 'Run the persistent macOS event worker in the foreground'),
            ('install', 'Install and start the user LaunchAgent'),
            ('status', 'Read durable index and queue status'),
            ('reconcile', 'Queue an explicit reconciliation for the running worker'),
            ('revert', 'Reverse a recovery journal while the worker is stopped')):
        sub = commands.add_parser(command, help=help_text)
        sub.add_argument('--config', help='JSON configuration path')
        scope = sub.add_mutually_exclusive_group()
        scope.add_argument('--root', action='append', help='Watch root; repeat to add roots')
        if command in ('scan', 'watch', 'install'):
            scope.add_argument('--all-user-files', action='store_true',
                               help='Discover user accounts, cloud documents and external volumes dynamically')
        sub.add_argument('--exclude', action='append', help='Additional excluded path; repeatable')
        sub.add_argument('--state-dir', help='Private SQLite, retry and pending-operation directory')
        sub.add_argument('--log-dir', help='Private recovery and diagnostic log directory')
        if command in ('scan', 'watch', 'install'):
            sub.add_argument('--apply', action='store_true', default=None, help='Enable filename changes')
        if command == 'reconcile':
            sub.add_argument('--path', action='append', help='Subtree to reconcile; defaults to roots')
        if command == 'revert':
            sub.add_argument('--journal', required=True, help='Recovery JSONL file (archives included)')
    commands.add_parser('stop', help='Stop the installed LaunchAgent until next login or install')
    commands.add_parser('uninstall', help='Remove the LaunchAgent; retain recovery records and state')
    return result


def configuration(args):
    loaded = False
    if args.config:
        result = Config.load(args.config)
        loaded = True
    elif args.command in ('status', 'reconcile', 'revert'):
        initial = Config(state_dir=args.state_dir) if args.state_dir else Config()
        path = Path(initial.state_dir) / 'config.json'
        result = Config.load(path) if path.exists() else initial
        loaded = path.exists()
    else:
        result = Config()
    values = asdict(result)
    for argument, field in (('root', 'roots'), ('state_dir', 'state_dir'), ('log_dir', 'log_dir')):
        if getattr(args, argument, None) is not None:
            values[field] = getattr(args, argument)
    if args.state_dir and not args.log_dir and not loaded:
        values['log_dir'] = None
    if args.exclude:
        values['excludes'] += args.exclude
    if getattr(args, 'all_user_files', False):
        values['scope'] = 'all-user-files'
        values['roots'] = []
        values['skip_hidden_tops'] = []
        if not loaded:
            values['excludes'] = args.exclude or []
    elif args.root:
        values['scope'] = 'configured'
    if getattr(args, 'apply', None) is not None:
        values['apply'] = args.apply
    # A scan is an inspection unless the current command explicitly asks to apply.
    if args.command == 'scan':
        values['apply'] = bool(args.apply)
    return Config(**values)


def output(value):
    print(json.dumps(value, ensure_ascii=False, indent=2))


def main(argv=None):
    args = parser().parse_args(argv)
    config = None
    try:
        from . import service
        if args.command == 'stop':
            result = service.stop_agent()
            if result.returncode:
                raise RuntimeError(result.stderr.strip() or 'agent is not loaded')
            output({'stopped': service.LABEL})
            return 0
        if args.command == 'uninstall':
            output({'removed': str(service.uninstall()), 'recovery_history_retained': True})
            return 0
        config = configuration(args)
        if args.command == 'install':
            output({'installed': str(service.install(config)), 'apply': config.apply})
        elif args.command == 'watch':
            service.watch(config)
        elif args.command == 'status':
            from .index import Index
            path = config.state_path('index.sqlite3')
            if not path.exists():
                output(dict(service.runtime_status(config), indexed=False,
                            message='No index exists; run watch or install first.'))
                return 0
            index = Index(path, read_only=True)
            try:
                output(dict(index.status(), **service.runtime_status(config)))
            finally:
                index.close()
        elif args.command == 'reconcile':
            from .index import Index
            from .sources import resolve_coverage, policy_for
            path = config.state_path('index.sqlite3')
            if not path.exists():
                raise RuntimeError('no index exists; start the worker first')
            index = Index(path)
            try:
                coverage = resolve_coverage(config)
                index.bind_policy(policy_for(config, coverage))
                index.request_reconcile(args.path)
                output({'queued': args.path or list(coverage.roots),
                        'worker_notified': service.signal_wakeup(config.state_path('wake.fifo'))})
            finally:
                index.close()
        elif args.command == 'revert':
            from .legacy import revert
            with service.runtime_lock(config):
                done, failed = revert(args.journal, str(Path(config.log_dir) / 'reverts.jsonl'))
            output({'reverted': done, 'failed': failed})
            return int(bool(failed))
        elif args.command == 'scan':
            from .normalizer import Normalizer
            from .sources import resolve_coverage, policy_for
            coverage = resolve_coverage(config)
            policy = policy_for(config, coverage)
            if config.apply:
                with service.runtime_lock(config):
                    normalizer = service.make_normalizer(config)
                    normalizer.policy = policy
                    try:
                        results = [normalizer.reconcile(root, True) for root in coverage.roots]
                    finally:
                        normalizer.close()
            else:
                normalizer = Normalizer(policy, None, None, None, apply=False)
                try:
                    results = [normalizer.reconcile(root, True) for root in coverage.roots]
                finally:
                    normalizer.close()
            import unicodedata
            entries = [entry for result in results for entry in result['entries']]
            candidates = [entry['path'] for entry in entries
                          if unicodedata.normalize('NFC', Path(entry['path']).name) != Path(entry['path']).name]
            errors = [error for result in results for error in result['errors']]
            output({'mode': 'apply' if config.apply else 'scan', 'entries': len(entries),
                    'candidates': candidates, 'renamed': sum(r['renamed'] for r in results),
                    'errors': errors})
            return int(bool(errors))
        return 0
    except Exception as exc:
        if args.command == 'watch':
            if config is None:
                support = (str(Path(args.config).expanduser().absolute().parent) if args.config
                           else args.state_dir or Config().state_dir)
                config = Config(state_dir=support, log_dir=args.log_dir)
            # Persist startup failures too, with the same bounded destination.
            Path(config.log_dir).mkdir(parents=True, exist_ok=True, mode=0o700)
            handler = RotatingFileHandler(Path(config.log_dir) / 'service.log',
                                          maxBytes=1024 * 1024, backupCount=3)
            handler.setFormatter(logging.Formatter('%(asctime)s %(levelname)s %(message)s'))
            record = logging.LogRecord('jaso_nfc', logging.ERROR, '', 0, str(exc), (), sys.exc_info())
            handler.handle(record)
            handler.close()
        print(f'jaso-nfc: {exc}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
