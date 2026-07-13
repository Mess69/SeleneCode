package com.example.controller;

import com.example.service.ArticleService;

@RestController
@RequestMapping("/articles")
public class ArticleController {

    private final ArticleService articleService;

    @Value("${app.greeting}")
    private String greeting;

    @GetMapping
    public String getAll() {
        return articleService.listArticles();
    }
}
