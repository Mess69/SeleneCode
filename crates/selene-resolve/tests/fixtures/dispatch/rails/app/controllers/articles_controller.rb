class ArticlesController < ApplicationController
  def index
    @articles = Article.recent
  end

  def create
    @article = Article.build_from(params)
  end
end
