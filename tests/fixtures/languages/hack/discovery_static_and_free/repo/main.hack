<?hh
function emit(): void {}
final class Box {
  public static function emit(): void {}
}
function run(): void {
  emit();
  Box::emit();
}
