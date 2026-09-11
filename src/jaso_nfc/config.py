"""Portable configuration; runtime data stays outside the source checkout."""

from dataclasses import asdict, dataclass, field, fields
import hashlib
import json
import os
from pathlib import Path
import tempfile


def support_directory():
    return str(Path.home() / 'Library/Application Support/jaso-nfc')


@dataclass
class Config:
    scope: str = 'configured'
    roots: list[str] = field(default_factory=lambda: [str(Path.home()), '/Users/Shared'])
    excludes: list[str] = field(default_factory=lambda: [
        str(Path.home() / 'Library'), str(Path.home() / '.Trash')])
    exclude_names: list[str] = field(default_factory=lambda: ['.git'])
    skip_hidden_tops: list[str] = field(default_factory=lambda: [str(Path.home())])
    state_dir: str = field(default_factory=support_directory)
    log_dir: str | None = None
    apply: bool = False

    def __post_init__(self):
        if self.scope not in ('configured', 'all-user-files'):
            raise ValueError('scope must be configured or all-user-files')
        if self.log_dir is None:
            self.log_dir = str(Path(self.state_dir) / 'logs')
        for name in ('roots', 'excludes', 'skip_hidden_tops'):
            values = getattr(self, name)
            if not isinstance(values, list) or any(not isinstance(v, str) or not v for v in values):
                raise ValueError(f'{name} must be a list of nonempty paths')
            setattr(self, name, list(dict.fromkeys(os.path.abspath(os.path.expanduser(v)) for v in values)))
        if not self.roots and self.scope == 'configured':
            raise ValueError('at least one root is required')
        if type(self.apply) is not bool:
            raise ValueError('apply must be a boolean')
        if not isinstance(self.exclude_names, list) or any(
                not isinstance(n, str) or not n or '/' in n for n in self.exclude_names):
            raise ValueError('exclude_names must contain directory basenames')
        for name in ('state_dir', 'log_dir'):
            value = getattr(self, name)
            if not isinstance(value, str) or not value:
                raise ValueError(f'{name} must be a nonempty path')
            setattr(self, name, os.path.abspath(os.path.expanduser(value)))

    @classmethod
    def load(cls, path):
        data = json.loads(Path(path).read_text(encoding='utf-8'))
        if not isinstance(data, dict) or set(data) - {f.name for f in fields(cls)}:
            raise ValueError('invalid configuration or unknown configuration fields')
        return cls(**data)

    def save(self, path):
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        fd, temporary = tempfile.mkstemp(dir=path.parent, prefix='.config-')
        try:
            with os.fdopen(fd, 'w', encoding='utf-8') as handle:
                json.dump(asdict(self), handle, ensure_ascii=False, indent=2)
                handle.write('\n')
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    def signature(self):
        return hashlib.sha256(json.dumps(asdict(self), sort_keys=True).encode()).hexdigest()

    def state_path(self, name):
        return Path(self.state_dir) / 'state' / name

    def policy(self):
        from .normalizer import Policy
        return Policy(self.roots, list(dict.fromkeys(self.excludes + [self.state_dir, self.log_dir])),
                      self.exclude_names, self.skip_hidden_tops)
