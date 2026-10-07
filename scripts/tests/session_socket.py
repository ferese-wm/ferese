"""Find the compositor endpoint in an isolated test runtime."""
from pathlib import Path


def ipc_socket(runtime):
    runtime = Path(runtime)
    default = runtime / "ferese/control.sock"
    instances = sorted(path for path in runtime.glob("ferese/instances/*/control.sock") if path.is_socket())
    if default.is_socket():
        instances.append(default)
    if len(instances) > 1:
        raise RuntimeError("Isolated test runtime contains multiple compositor instances")
    return instances[0] if instances else default


def ipc_environment(environment):
    return dict(environment, FERESE_SOCKET=str(ipc_socket(environment["XDG_RUNTIME_DIR"])))
