from .compiler import SQLCompiler

class ModelIterable:
    def __init__(self, queryset):
        self.queryset = queryset

    def __iter__(self):
        compiler = SQLCompiler()
        return iter(compiler.execute_sql())
