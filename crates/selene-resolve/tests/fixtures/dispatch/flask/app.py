from flask import Flask, request

from services import create_article

app = Flask(__name__)


@app.route('/articles', methods=['POST'])
@login_required
def create():
    return create_article(request.json)
