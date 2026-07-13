from .services import get_article

class ArticleDetail:
    def get(self, request, slug):
        return get_article(slug)

class ArticleViewSet:
    def list(self, request):
        return get_article(None)

def article_detail(request, pk):
    return get_article(pk)
