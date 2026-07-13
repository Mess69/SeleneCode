import React from 'react';

interface Props {
  title: string;
}

export class Panel extends React.Component<Props> {
  render() {
    return <div>{this.props.title}</div>;
  }
}
