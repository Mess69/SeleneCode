class Base:
    def handle(self):
        return 0


class Mixin:
    pass


class Child(Base, Mixin):
    def handle(self):
        return 1
