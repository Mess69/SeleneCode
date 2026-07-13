export interface Serializable {
  serialize(): string;
}

export interface Repo extends Serializable {
  find(id: string): void;
}

class BaseController {
  handle(): void {}
}

export class ChildController extends BaseController implements Serializable {
  serialize(): string {
    return '';
  }
}
