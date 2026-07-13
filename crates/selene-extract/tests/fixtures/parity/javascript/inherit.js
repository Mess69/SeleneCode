class Animal {
  speak() {
    return 'generic';
  }
}

export class Dog extends Animal {
  speak() {
    return 'woof';
  }
}
