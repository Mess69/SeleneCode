from django.urls import path, re_path, include
from rest_framework import routers
from . import views
from .views import ArticleDetail, ArticleViewSet

router = routers.DefaultRouter()
router.register(r'articles', ArticleViewSet)

# path('dead/', DeadView.as_view()),
urlpatterns = [
    path('articles/<slug>/', ArticleDetail.as_view()),
    path('legacy/', views.article_detail),
    re_path(r'^old/(?P<pk>\d+)/$', views.article_detail),
    path('api/', include('api.urls')),
]
