import { useArticle } from './hooks/useArticle';

export default function Article() {
  const data = useArticle();
  return <div>{data}</div>;
}
