import { Route } from 'react-router-dom';
import Article from './Article';

export function Legacy() {
  return <Route path="/v5/article" component={Article} />;
}
