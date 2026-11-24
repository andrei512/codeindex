class Worker
  def self.emit
  end
  def self.flush
  end
  def self.run
    Worker.flush()
  end
end
class Other
  def self.emit
  end
end
