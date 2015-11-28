package dora

import (
	"github.com/bnagy/gapstone"
	"github.com/fatih/color"
	w "github.com/williballenthin/Lancelot/workspace"
	"log"
)

func check(e error) {
	if e != nil {
		panic(e)
	}
}

// dora the explora
type Dora struct {
	ws *w.Workspace
	ac ArtifactCollection
}

func New(ws *w.Workspace) (*Dora, error) {
	// TODO: get this from a real place
	ac, e := NewLoggingArtifactCollection()
	check(e)

	return &Dora{
		ws: ws,
		ac: ac,
	}, nil
}

func isBBEnd(insn gapstone.Instruction) bool {
	return w.DoesInstructionHaveGroup(insn, gapstone.X86_GRP_CALL) ||
		w.DoesInstructionHaveGroup(insn, gapstone.X86_GRP_JUMP) ||
		w.DoesInstructionHaveGroup(insn, gapstone.X86_GRP_RET) ||
		w.DoesInstructionHaveGroup(insn, gapstone.X86_GRP_IRET)
}

func GetNextInstructionPointer(emu *w.Emulator, sman *w.SnapshotManager) (w.VA, error) {
	var va w.VA
	e := sman.WithTempExcursion(func() error {
		e := emu.StepInto()
		if e != nil {
			return e
		}
		va = emu.GetInstructionPointer()
		return nil
	})
	return va, e
}

// things yet to discover:
//   OK: final stack delta
//   TODO: arguments passed in registers
//   TODO: arguments passed on stack
//   TODO: all basic blocks
//   TODO: calling convention
//   TODO: no return functions
// TODO: ensure stack is set up with return pointer and some junk symbolic args
// TODO: track max hits
// this is going to be a pretty wild function :-(
func (dora *Dora) ExploreFunction(va w.VA) error {
	emu, e := dora.ws.GetEmulator()
	check(e)
	defer emu.Close()

	sman, e := w.NewSnapshotManager(emu)
	check(e)
	defer sman.Close()

	bbStart := va
	emu.SetInstructionPointer(va)
	check(e)

	beforeSp := emu.GetStackPointer()

	// TODO: how to disable these while on an excursion?
	rh, e := emu.HookMemRead(func(access int, addr w.VA, size int, value int64) {
		dora.ac.AddMemoryReadXref(MemoryReadCrossReference{emu.GetInstructionPointer(), addr})
	})
	check(e)
	defer rh.Close()

	wh, e := emu.HookMemWrite(func(access int, addr w.VA, size int, value int64) {
		dora.ac.AddMemoryWriteXref(MemoryWriteCrossReference{emu.GetInstructionPointer(), addr})
	})
	check(e)
	defer wh.Close()

	for {
		s, _, e := emu.FormatAddress(emu.GetInstructionPointer())
		check(e)
		color.Set(color.FgHiBlack)
		log.Printf("ip:" + s)
		color.Unset()

		insn, e := emu.GetCurrentInstruction()
		check(e)

		if w.DoesInstructionHaveGroup(insn, gapstone.X86_GRP_CALL) {
			// TODO: have to wire up import detection
			nextPc, e := GetNextInstructionPointer(emu, sman)
			if e == nil {
				log.Printf("  call target: 0x%x", nextPc)
			}

			dora.ac.AddCallXref(CallCrossReference{emu.GetInstructionPointer(), nextPc})
		}

		if w.DoesInstructionHaveGroup(insn, gapstone.X86_GRP_RET) ||
			w.DoesInstructionHaveGroup(insn, gapstone.X86_GRP_IRET) {
			log.Printf("returning, done.")
			afterSp := emu.GetStackPointer()
			stackDelta := uint64(afterSp) - uint64(beforeSp)
			log.Printf("stack delta: 0x%x", stackDelta)
			break
		}

		if isBBEnd(insn) {
			e := dora.ac.AddBasicBlock(BasicBlock{Start: bbStart, End: emu.GetInstructionPointer()})
			check(e)
		}

		beforePc := emu.GetInstructionPointer()
		e = emu.StepOver()
		if e != nil {
			log.Printf("error: %s", e.Error())
			break
		}
		afterPc := emu.GetInstructionPointer()

		// TODO: need to detect calling convention, and in the case of stdcall,
		//   cleanup the stack

		if isBBEnd(insn) {
			bbStart = emu.GetInstructionPointer()
			e := dora.ac.AddJumpXref(JumpCrossReference{beforePc, afterPc})
			check(e)
		}
	}

	/*
		snap, e := dora.emu.Snapshot()
		check(e)

		defer func() {
			e := dora.emu.RestoreSnapshot(snap)
			check(e)

			e = dora.emu.UnhookSnapshot(snap)
			check(e)
		}()
	*/

	return nil
}
