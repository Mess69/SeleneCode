package main

import (
	"example.com/blog/handlers"
	"github.com/gin-gonic/gin"
)

func main() {
	r := gin.Default()
	v1 := r.Group("/api/v1")
	v1.POST("/articles", handlers.CreateArticle)
	r.Run()
}
