"""Load and validate a JSON config file with defaults."""
import json
import os


DEFAULT_CONFIG = {
    "host": "localhost",
    "port": 8080,
    "debug": False,
    "max_connections": 100,
    "timeout": 30,
}


def load_config(path):
    """Load config from JSON file, merging with defaults.

    Returns the merged config dict.
    Raises ConfigError for any problems.
    """
    config = dict(DEFAULT_CONFIG)

    if not os.path.exists(path):
        return config

    try:
        with open(path) as f:
            data = json.load(f)
    except json.JSONDecodeError:
        raise ConfigError(f"Invalid JSON in {path}")

    if not isinstance(data, dict):
        raise ConfigError(f"Config must be a JSON object, got {type(data).__name__}")

    config.update(data)
    validate_config(config)
    return config


def validate_config(config):
    """Validate config values."""
    port = config["port"]
    if not isinstance(port, int) or port < 1 or port > 65535:
        raise ConfigError(f"Invalid port: {port}")

    timeout = config["timeout"]
    if not isinstance(timeout, int) or timeout < 0:
        raise ConfigError(f"Invalid timeout: {timeout}")

    max_conn = config["max_connections"]
    if not isinstance(max_conn, int) or max_conn < 1:
        raise ConfigError(f"Invalid max_connections: {max_conn}")


class ConfigError(Exception):
    pass
