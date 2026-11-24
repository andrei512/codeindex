<?hh
function emit(): void {}
function flush(): void {}
function run_work(): void {
  emit();
}
final class Other {
  public static function emit(): void {}
}
