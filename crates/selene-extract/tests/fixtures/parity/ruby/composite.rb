require 'json'
require_relative 'helper'

module Store
  class Repository
    def initialize(db)
      @db = db
    end

    def find(id)
      raw = @db.query(id)
      JSON.parse(raw)
    end

    def self.build(db)
      new(db)
    end
  end
end
