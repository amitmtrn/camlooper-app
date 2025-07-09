import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Check, Download, Heart } from "lucide-react";

export const Pricing = () => {
  const features = [
    "Unlimited video recordings",
    "Virtual camera creation",
    "Seamless video looping",
    "All major format support",
    "Local processing (privacy-first)",
    "Universal app compatibility",
    "Regular updates",
    "Community support"
  ];

  return (
    <section id="pricing" className="py-24 relative">
      <div className="absolute inset-0 bg-gradient-to-b from-background to-background/50" />
      
      <div className="container mx-auto px-4 relative z-10">
        <div className="text-center space-y-4 mb-16">
          <h2 className="text-3xl md:text-5xl font-bold">
            Simple,{" "}
            <span className="bg-gradient-primary bg-clip-text text-transparent">
              Transparent
            </span>{" "}
            Pricing
          </h2>
          <p className="text-xl text-muted-foreground max-w-2xl mx-auto">
            CamLooper is completely free to use, supported by unobtrusive ads
          </p>
        </div>

        <div className="max-w-md mx-auto">
          <Card className="p-8 bg-gradient-card border-primary/40 shadow-glow relative overflow-hidden">
            {/* Glow Effect */}
            <div className="absolute inset-0 bg-gradient-primary opacity-5" />
            
            <div className="relative z-10 text-center space-y-6">
              {/* Badge */}
              <div className="inline-flex items-center space-x-2 bg-primary/10 rounded-full px-4 py-2 border border-primary/20">
                <Heart className="h-4 w-4 text-primary" />
                <span className="text-sm text-primary font-medium">Community Favorite</span>
              </div>

              {/* Price */}
              <div>
                <div className="text-5xl font-bold bg-gradient-primary bg-clip-text text-transparent">
                  FREE
                </div>
                <div className="text-muted-foreground">Forever & Always</div>
              </div>

              {/* Features */}
              <div className="space-y-3 text-left">
                {features.map((feature, index) => (
                  <div key={index} className="flex items-center space-x-3">
                    <div className="w-5 h-5 bg-primary/10 rounded-full flex items-center justify-center">
                      <Check className="h-3 w-3 text-primary" />
                    </div>
                    <span className="text-foreground">{feature}</span>
                  </div>
                ))}
              </div>

              {/* CTA */}
              <Button variant="hero" size="lg" className="w-full group">
                <Download className="h-5 w-5 group-hover:scale-110 transition-transform" />
                Download CamLooper
              </Button>

              {/* Ad Support Note */}
              <div className="text-sm text-muted-foreground border-t border-border pt-4">
                <p>
                  Supported by minimal, unobtrusive ads that don't interfere with your workflow. 
                  Your privacy and user experience come first.
                </p>
              </div>
            </div>
          </Card>
        </div>

        {/* Additional Info */}
        <div className="mt-16 text-center space-y-4">
          <h3 className="text-2xl font-semibold">Why Free?</h3>
          <p className="text-muted-foreground max-w-2xl mx-auto leading-relaxed">
            We believe powerful creative tools should be accessible to everyone. CamLooper is funded 
            through carefully placed, non-intrusive advertisements that respect your creative process.
          </p>
        </div>
      </div>
    </section>
  );
};