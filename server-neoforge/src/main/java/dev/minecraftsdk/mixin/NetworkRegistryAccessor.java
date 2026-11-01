package dev.minecraftsdk.mixin;
import java.util.Map;
import net.minecraft.network.ConnectionProtocol;
import net.minecraft.resources.ResourceLocation;
import net.neoforged.neoforge.network.registration.*;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;
@Mixin(NetworkRegistry.class)
public interface NetworkRegistryAccessor {
    @Accessor("PAYLOAD_REGISTRATIONS")
    static Map<ConnectionProtocol,Map<ResourceLocation,PayloadRegistration<?>>> sdk$registrations(){throw new AssertionError("Mixin not applied");}
}
