import os
from typing import List

from .models import User


class Repo:
    """Storage."""

    def __init__(self, db):
        self.db = db

    def all(self) -> List[User]:
        return self.db.query(User)


def main():
    repo = Repo(connect())
    return repo.all()
