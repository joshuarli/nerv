"""Tests for the todo list core."""

import pytest
from todo import TodoList


def test_add_and_list():
    t = TodoList()
    item = t.add("buy milk")
    assert item.id == 1
    assert item.text == "buy milk"
    assert not item.done
    assert len(t.all()) == 1


def test_complete():
    t = TodoList()
    t.add("write tests")
    item = t.complete(1)
    assert item is not None
    assert item.done
    assert t.pending() == []


def test_remove():
    t = TodoList()
    t.add("a")
    t.add("b")
    assert t.remove(1)
    assert len(t.all()) == 1
    assert t.all()[0].text == "b"


def test_pending_excludes_done():
    t = TodoList()
    t.add("x")
    t.add("y")
    t.complete(1)
    assert len(t.pending()) == 1
    assert t.pending()[0].text == "y"
