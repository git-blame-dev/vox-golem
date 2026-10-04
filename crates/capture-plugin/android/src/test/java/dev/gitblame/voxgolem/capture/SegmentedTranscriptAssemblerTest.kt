package dev.gitblame.voxgolem.capture

import org.junit.Assert.assertEquals
import org.junit.Test

class SegmentedTranscriptAssemblerTest {
    @Test
    fun committedSegmentsAndChangingPartialProduceOneCompleteFinalHypothesis() {
        val transcript = SegmentedTranscriptAssembler()
        assertEquals("first phrase", transcript.commit("first phrase"))
        assertEquals("first phrase unfinished", transcript.hypothesis("unfinished"))
        assertEquals("first phrase second phrase", transcript.commit("second phrase"))
        assertEquals("first phrase second phrase complete", transcript.hypothesis("complete"))
        assertEquals("first phrase second phrase complete", transcript.finish("complete"))
        assertEquals("first phrase second phrase", transcript.finish(""))
    }
}
