import React, { useState } from 'react';
import { helper } from '../utils/helper';

export class Panel extends React.Component<Props> {
  render() {
    return helper(this.props.value);
  }
}

export const Button = ({ label }: Props) => {
  const [count, setCount] = useState(0);
  return <button onClick={() => setCount(count + 1)}>{label}</button>;
};
