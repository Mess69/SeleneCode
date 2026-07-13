import json
from typing import List, Dict, Optional


class Repository:
    """Persistence for users."""

    def __init__(self, db):
        self.db = db

    @staticmethod
    def parse(raw: str) -> Dict:
        return json.loads(raw)

    def all(self) -> List[Dict]:
        rows = self.db.query("SELECT * FROM users")
        return [Repository.parse(r) for r in rows]


def build(db) -> Repository:
    return Repository(db)
