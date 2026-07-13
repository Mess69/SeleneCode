package handlers

import (
	"example.com/blog/service"
	"github.com/gin-gonic/gin"
)

func CreateArticle(c *gin.Context) {
	service.Create()
}
