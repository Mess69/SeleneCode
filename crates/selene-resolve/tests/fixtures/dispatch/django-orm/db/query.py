from .iterables import ModelIterable

class QuerySet:
    def __init__(self):
        self._iterable_class = ModelIterable
        self._result_cache = None

    def _fetch_all(self):
        if self._result_cache is None:
            self._result_cache = list(self._iterable_class(self))
        self._prefetch_related_objects()

    def _prefetch_related_objects(self):
        return None
