from fastapi import FastAPI

from services import list_articles

app = FastAPI()


@app.get("/articles")
async def index():
    return list_articles()
