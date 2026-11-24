<?hh
function emit(): void {}
function flush(): void {}
function run_work(): void {
  flush();
}
final class Other {
  public static function emit(): void {}
}
