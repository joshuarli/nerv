"""Simple in-memory todo list."""

from dataclasses import dataclass, field
from typing import Optional


@dataclass
class Item:
    id: int
    text: str
    done: bool = False


class TodoList:
    def __init__(self):
        self._items: list[Item] = []
        self._next_id: int = 1

    def add(self, text: str) -> Item:
        item = Item(id=self._next_id, text=text)
        self._items.append(item)
        self._next_id += 1
        return item

    def complete(self, item_id: int) -> Optional[Item]:
        for item in self._items:
            if item.id == item_id:
                item.done = True
                return item
        return None

    def remove(self, item_id: int) -> bool:
        before = len(self._items)
        self._items = [i for i in self._items if i.id != item_id]
        return len(self._items) < before

    def all(self) -> list[Item]:
        return list(self._items)

    def pending(self) -> list[Item]:
        return [i for i in self._items if not i.done]
