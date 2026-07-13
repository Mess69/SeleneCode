<?php

namespace App\Http\Controllers;

use App\Services\ArticleService;

class ArticleController
{
    private ArticleService $articleService;

    public function index()
    {
        return $this->articleService->listArticles();
    }

    public function store()
    {
        return $this->articleService->createArticle();
    }
}
