# decorated function
@app.route("/x")
def py_handler():
    return 1


# plain function control
def py_plain():
    return 1


# decorated class
@dataclass
class PyModel:
    pass
