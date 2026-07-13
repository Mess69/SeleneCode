class QuerySet:
    def _fetch_all(self):
        return list(self._iterable_class(self))
