import { useArticle } from '../../hooks/useArticle';

export default function ArticlePage() {
  return <div>{useArticle()}</div>;
}
