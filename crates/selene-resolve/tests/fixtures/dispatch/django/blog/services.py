from .models import Article

def get_article(slug):
    return Article.objects.filter(slug=slug)
