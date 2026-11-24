class Worker
  def self.emit
  end
  def self.flush
  end
  def self.run
    Worker.emit()
  end
end
class Other
  def self.emit
  end
end
