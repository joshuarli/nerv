import json
import os
import tempfile
from config_loader import load_config, ConfigError


def write_json(path, data):
    with open(path, "w") as f:
        json.dump(data, f)


def test_defaults():
    config = load_config("/nonexistent/path.json")
    assert config["host"] == "localhost"
    assert config["port"] == 8080


def test_merge():
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        json.dump({"port": 3000, "debug": True}, f)
        path = f.name
    try:
        config = load_config(path)
        assert config["port"] == 3000
        assert config["debug"] is True
        assert config["host"] == "localhost"  # default preserved
    finally:
        os.unlink(path)


def test_invalid_json():
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        f.write("{not valid json")
        path = f.name
    try:
        try:
            load_config(path)
            assert False, "should have raised ConfigError"
        except ConfigError as e:
            assert "Invalid JSON" in str(e)
    finally:
        os.unlink(path)


def test_not_a_dict():
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        json.dump([1, 2, 3], f)
        path = f.name
    try:
        try:
            load_config(path)
            assert False, "should have raised ConfigError"
        except ConfigError as e:
            assert "object" in str(e)
    finally:
        os.unlink(path)


def test_invalid_port():
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        json.dump({"port": -1}, f)
        path = f.name
    try:
        try:
            load_config(path)
            assert False, "should have raised ConfigError"
        except ConfigError:
            pass
    finally:
        os.unlink(path)


def test_permission_denied():
    """Config file exists but is unreadable — should raise ConfigError, not crash."""
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        json.dump({"port": 3000}, f)
        path = f.name
    try:
        os.chmod(path, 0o000)
        try:
            load_config(path)
            assert False, "should have raised ConfigError"
        except ConfigError as e:
            assert "read" in str(e).lower() or "permission" in str(e).lower()
    finally:
        os.chmod(path, 0o644)
        os.unlink(path)


def test_string_port_rejected():
    """Port given as string should raise ConfigError."""
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        json.dump({"port": "8080"}, f)
        path = f.name
    try:
        try:
            load_config(path)
            assert False, "should have raised ConfigError"
        except ConfigError:
            pass
    finally:
        os.unlink(path)


if __name__ == "__main__":
    test_defaults()
    test_merge()
    test_invalid_json()
    test_not_a_dict()
    test_invalid_port()
    test_permission_denied()
    test_string_port_rejected()
    print("All tests passed")
