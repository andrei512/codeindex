class Job
  def self.visible(); 1; end
end
Job.visible()
__END__
class Phantom
  def self.decoy(); Job.visible(); end
end
