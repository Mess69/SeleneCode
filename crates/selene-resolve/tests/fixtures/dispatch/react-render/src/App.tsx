import { StaticCanvas } from './StaticCanvas';

export class App {
  state = { scene: null };

  handleClick() {
    this.setState({ scene: 1 });
  }

  helper() {
    return 42;
  }

  render() {
    return (
      <div>
        <StaticCanvas scene={this.state.scene} />
        <span>plain dom</span>
      </div>
    );
  }
}
